use crate::descriptor::Descriptor;
use crate::wit::{AdapterType, Instruction, InstructionBuilder};
use crate::wit::{InstructionData, StackChange};
use anyhow::{bail, format_err, Error};
use walrus::ValType;

impl InstructionBuilder<'_, '_> {
    /// Processes one more `Descriptor` as an argument to a JS function that
    /// Wasm is calling.
    ///
    /// This will internally skip `Unit` and otherwise build up the `bindings`
    /// map and ensure that it's correctly mapped from Wasm to JS.
    pub fn outgoing(&mut self, arg: &Descriptor) -> Result<(), Error> {
        if let Descriptor::Unit = arg {
            return Ok(());
        }
        // Similar rationale to `incoming.rs` around these sanity checks.
        let input_before = self.input.len();
        let output_before = self.output.len();
        self._outgoing(arg)?;

        assert!(input_before < self.input.len());
        if let Descriptor::Result(arg) = arg {
            if let Descriptor::Unit = &**arg {
                assert_eq!(output_before, self.output.len());
                return Ok(());
            }
        }
        assert_eq!(output_before + 1, self.output.len());
        Ok(())
    }

    fn _outgoing(&mut self, arg: &Descriptor) -> Result<(), Error> {
        match arg {
            Descriptor::Boolean => {
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::BoolFromI32,
                    &[AdapterType::Bool],
                );
            }
            Descriptor::Externref => {
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::ExternrefLoadOwned {
                        table_and_drop: None,
                    },
                    &[AdapterType::Externref],
                );
            }
            Descriptor::NamedExternref(name) => {
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::ExternrefLoadOwned {
                        table_and_drop: None,
                    },
                    &[AdapterType::NamedExternref(name.clone())],
                );
            }
            Descriptor::I8 => self.outgoing_i32(AdapterType::S8),
            Descriptor::U8 => self.outgoing_i32(AdapterType::U8),
            Descriptor::I16 => self.outgoing_i32(AdapterType::S16),
            Descriptor::U16 => self.outgoing_i32(AdapterType::U16),
            Descriptor::I32 => self.outgoing_i32(AdapterType::S32),
            Descriptor::U32 => self.outgoing_i32(AdapterType::U32),
            Descriptor::I64 => self.outgoing_i64(AdapterType::I64),
            Descriptor::U64 => self.outgoing_i64(AdapterType::U64),
            Descriptor::I128 => {
                self.instruction(
                    &[AdapterType::I64, AdapterType::I64],
                    Instruction::WasmToInt128 { signed: true },
                    &[AdapterType::S128],
                );
            }
            Descriptor::U128 => {
                self.instruction(
                    &[AdapterType::I64, AdapterType::I64],
                    Instruction::WasmToInt128 { signed: false },
                    &[AdapterType::U128],
                );
            }
            Descriptor::F32 => {
                self.get(AdapterType::F32);
                self.output.push(AdapterType::F32);
            }
            Descriptor::F64 => {
                self.get(AdapterType::F64);
                self.output.push(AdapterType::F64);
            }
            Descriptor::Enum { name, .. } => self.outgoing_i32(AdapterType::Enum(name.clone())),
            Descriptor::StringEnum { name, .. } => self.outgoing_string_enum(name),

            Descriptor::Char => {
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::StringFromChar,
                    &[AdapterType::String],
                );
            }

            Descriptor::RustStruct(class) => {
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::RustFromI32 {
                        class: class.to_string(),
                    },
                    &[AdapterType::Struct(class.clone())],
                );
            }
            Descriptor::Ref(d) => self.outgoing_ref(false, d)?,
            Descriptor::RefMut(d) => self.outgoing_ref(true, d)?,

            Descriptor::CachedString => self.cached_string(true)?,

            Descriptor::String => {
                // fetch the ptr/length ...
                self.get(AdapterType::I32);
                self.get(AdapterType::I32);

                // ... then defer a call to `free` to happen later
                let free = self.cx.free()?;
                self.instructions.push(InstructionData {
                    instr: Instruction::DeferFree { free, align: 1 },
                    stack_change: StackChange::Modified {
                        popped: 2,
                        pushed: 2,
                    },
                });

                // ... and then convert it to a string type
                self.instructions.push(InstructionData {
                    instr: Instruction::MemoryToString(self.cx.memory()?),
                    stack_change: StackChange::Modified {
                        popped: 2,
                        pushed: 1,
                    },
                });
                self.output.push(AdapterType::String);
            }

            Descriptor::Vector(_) => {
                let kind = arg.vector_kind().ok_or_else(|| {
                    format_err!(
                        "unsupported argument type for calling JS function from Rust {:?}",
                        arg
                    )
                })?;
                let mem = self.cx.memory()?;
                let free = self.cx.free()?;
                self.instruction(
                    &[AdapterType::I32, AdapterType::I32],
                    Instruction::VectorLoad {
                        kind: kind.clone(),
                        mem,
                        free,
                    },
                    &[AdapterType::Vector(kind)],
                );
            }

            Descriptor::Option(d) => self.outgoing_option(d)?,
            Descriptor::Result(d) => self.outgoing_result(d)?,

            Descriptor::Function(_) | Descriptor::Closure(_) | Descriptor::Slice(_) => bail!(
                "unsupported argument type for calling JS function from Rust: {:?}",
                arg
            ),

            // nothing to do
            Descriptor::Unit => {}

            // Largely synthetic and can't show up
            Descriptor::ClampedU8 => unreachable!(),

            Descriptor::NonNull => self.outgoing_i32(AdapterType::NonNull),

            // canot be reached
            Descriptor::Generic { .. } => unreachable!(),
        }
        Ok(())
    }

    fn outgoing_ref(&mut self, mutable: bool, arg: &Descriptor) -> Result<(), Error> {
        match arg {
            Descriptor::Externref => {
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::TableGet,
                    &[AdapterType::Externref],
                );
            }
            Descriptor::NamedExternref(name) => {
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::TableGet,
                    &[AdapterType::NamedExternref(name.clone())],
                );
            }
            Descriptor::CachedString => self.cached_string(false)?,

            Descriptor::String => {
                self.instruction(
                    &[AdapterType::I32, AdapterType::I32],
                    Instruction::MemoryToString(self.cx.memory()?),
                    &[AdapterType::String],
                );
            }
            Descriptor::Slice(_) => {
                let kind = arg.vector_kind().ok_or_else(|| {
                    format_err!(
                        "unsupported argument type for calling JS function from Rust {:?}",
                        arg
                    )
                })?;
                let mem = self.cx.memory()?;
                self.instruction(
                    &[AdapterType::I32, AdapterType::I32],
                    Instruction::View {
                        kind: kind.clone(),
                        mem,
                    },
                    &[AdapterType::Vector(kind)],
                );
            }

            Descriptor::Function(descriptor) => {
                // synthesize the a/b arguments that aren't present in the
                // signature from wasm-bindgen but are present in the Wasm file.
                let mut descriptor = (**descriptor).clone();
                let nargs = descriptor.arguments.len();
                descriptor.arguments.insert(0, Descriptor::I32);
                descriptor.arguments.insert(0, Descriptor::I32);
                let adapter = self
                    .cx
                    .table_element_adapter(descriptor.shim_idx, descriptor)?;
                self.instruction(
                    &[AdapterType::I32, AdapterType::I32],
                    Instruction::StackClosure {
                        adapter,
                        nargs,
                        mutable,
                    },
                    &[AdapterType::Function],
                );
            }

            _ => bail!(
                "unsupported reference argument type for calling JS function from Rust: {:?}",
                arg
            ),
        }
        Ok(())
    }

    fn outgoing_option(&mut self, arg: &Descriptor) -> Result<(), Error> {
        match arg {
            Descriptor::Externref => {
                // This is set to `undefined` in the `None` case and otherwise
                // is the valid owned index.
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::ExternrefLoadOwned {
                        table_and_drop: None,
                    },
                    &[AdapterType::Externref.option()],
                );
            }
            Descriptor::NamedExternref(name) => {
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::ExternrefLoadOwned {
                        table_and_drop: None,
                    },
                    &[AdapterType::NamedExternref(name.clone()).option()],
                );
            }
            Descriptor::I8 => self.out_option_sentinel32(AdapterType::S8),
            Descriptor::U8 => self.out_option_sentinel32(AdapterType::U8),
            Descriptor::I16 => self.out_option_sentinel32(AdapterType::S16),
            Descriptor::U16 => self.out_option_sentinel32(AdapterType::U16),
            Descriptor::I32 => self.out_option_sentinel64(AdapterType::S32),
            Descriptor::U32 => self.out_option_sentinel64(AdapterType::U32),
            Descriptor::I64 => self.option_native(true, ValType::I64),
            Descriptor::U64 => self.option_native(false, ValType::I64),
            Descriptor::F32 => self.out_option_sentinel64(AdapterType::F32),
            Descriptor::F64 => self.option_native(true, ValType::F64),
            Descriptor::I128 => {
                self.instruction(
                    &[AdapterType::I32, AdapterType::I64, AdapterType::I64],
                    Instruction::OptionWasmToInt128 { signed: true },
                    &[AdapterType::S128.option()],
                );
            }
            Descriptor::U128 => {
                self.instruction(
                    &[AdapterType::I32, AdapterType::I64, AdapterType::I64],
                    Instruction::OptionWasmToInt128 { signed: false },
                    &[AdapterType::U128.option()],
                );
            }
            Descriptor::Boolean => {
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::OptionBoolFromI32,
                    &[AdapterType::Bool.option()],
                );
            }
            Descriptor::Char => {
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::OptionCharFromI32,
                    &[AdapterType::String.option()],
                );
            }
            Descriptor::Enum { name, hole } => {
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::OptionEnumFromI32 { hole: *hole },
                    &[AdapterType::Enum(name.clone()).option()],
                );
            }
            Descriptor::StringEnum { name, .. } => {
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::OptionWasmToStringEnum { name: name.clone() },
                    &[AdapterType::StringEnum(name.clone()).option()],
                );
            }
            Descriptor::RustStruct(name) => {
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::OptionRustFromI32 {
                        class: name.to_string(),
                    },
                    &[AdapterType::Struct(name.clone()).option()],
                );
            }
            Descriptor::Ref(d) => self.outgoing_option_ref(false, d)?,
            Descriptor::RefMut(d) => self.outgoing_option_ref(true, d)?,

            Descriptor::CachedString => self.cached_string(true)?,

            Descriptor::String | Descriptor::Vector(_) => {
                let kind = arg.vector_kind().ok_or_else(|| {
                    format_err!(
                        "unsupported optional slice type for calling JS function from Rust {:?}",
                        arg
                    )
                })?;
                let mem = self.cx.memory()?;
                let free = self.cx.free()?;
                self.instruction(
                    &[AdapterType::I32, AdapterType::I32],
                    Instruction::OptionVectorLoad {
                        kind: kind.clone(),
                        mem,
                        free,
                    },
                    &[AdapterType::Vector(kind).option()],
                );
            }

            Descriptor::NonNull => self.instruction(
                &[AdapterType::I32],
                Instruction::OptionNonNullFromI32,
                &[AdapterType::NonNull.option()],
            ),

            _ => bail!(
                "unsupported optional argument type for calling JS function from Rust: {:?}",
                arg
            ),
        }
        Ok(())
    }

    fn outgoing_result(&mut self, arg: &Descriptor) -> Result<(), Error> {
        match arg {
            Descriptor::Externref
            | Descriptor::NamedExternref(_)
            | Descriptor::I8
            | Descriptor::U8
            | Descriptor::I16
            | Descriptor::U16
            | Descriptor::I32
            | Descriptor::U32
            | Descriptor::F32
            | Descriptor::F64
            | Descriptor::I64
            | Descriptor::U64
            | Descriptor::I128
            | Descriptor::U128
            | Descriptor::Boolean
            | Descriptor::Char
            | Descriptor::Enum { .. }
            | Descriptor::StringEnum { .. }
            | Descriptor::RustStruct(_)
            | Descriptor::Ref(_)
            | Descriptor::RefMut(_)
            | Descriptor::CachedString
            | Descriptor::Option(_)
            | Descriptor::Vector(_)
            | Descriptor::Unit
            | Descriptor::NonNull => {
                // We must throw before reading the Ok type, if there is an error. However, the
                // structure of ResultAbi is that the Err value + discriminant come last (for
                // alignment reasons). So the UnwrapResult instruction must come first, but the
                // inputs must be read last.
                //
                // So first, push an UnwrapResult instruction without modifying the inputs list.
                //
                //     []
                //     -------------------------<
                //     UnwrapResult { popped: 2 }
                //
                self.instructions.push(InstructionData {
                    instr: Instruction::UnwrapResult {
                        table_and_drop: None,
                    },
                    stack_change: StackChange::Modified {
                        popped: 2,
                        pushed: 0,
                    },
                });

                // Then push whatever else you were going to do, modifying the inputs and
                // instructions.
                //
                //     [f64, u32, u32]
                //     -------------------------<
                //     UnwrapResult { popped: 2 }
                //     SomeOtherInstruction { popped: 3 }
                //
                // The popped numbers don't add up yet (3 != 5), but they will.
                let len = self.instructions.len();
                self._outgoing(arg)?;

                // check we did not add any deferred calls, because we have undermined the idea of
                // running them unconditionally in a finally {} block. String does this, but we
                // special case it.
                assert!(!self.instructions[len..]
                    .iter()
                    .any(|idata| matches!(idata.instr, Instruction::DeferFree { .. })));

                // Finally, we add the two inputs to UnwrapResult, and everything checks out
                //
                //     [f64, u32, u32, u32, u32]
                //     -------------------------<
                //     UnwrapResult { popped: 2 }
                //     SomeOtherInstruction { popped: 3 }
                //
                self.get(AdapterType::I32);
                self.get(AdapterType::I32);
            }
            Descriptor::String => {
                // fetch the ptr/length ...
                self.get(AdapterType::I32);
                self.get(AdapterType::I32);
                // fetch the err/is_err
                self.get(AdapterType::I32);
                self.get(AdapterType::I32);

                self.instructions.push(InstructionData {
                    instr: Instruction::UnwrapResultString {
                        table_and_drop: None,
                    },
                    stack_change: StackChange::Modified {
                        // 2 from UnwrapResult, 2 from ptr/len
                        popped: 4,
                        // pushes the ptr/len back on
                        pushed: 2,
                    },
                });

                // ... then defer a call to `free` to happen later
                // this will run string's DeferCallCore with the length parameter, but if is_err,
                // then we have never written anything into that, so it is poison. So we'll have to
                // make sure we call it with length 0, which according to __wbindgen_free's
                // implementation is always safe. We do this in UnwrapResultString's
                // implementation.
                let free = self.cx.free()?;
                self.instructions.push(InstructionData {
                    instr: Instruction::DeferFree { free, align: 1 },
                    stack_change: StackChange::Modified {
                        popped: 2,
                        pushed: 2,
                    },
                });

                // ... and then convert it to a string type
                self.instructions.push(InstructionData {
                    instr: Instruction::MemoryToString(self.cx.memory()?),
                    stack_change: StackChange::Modified {
                        popped: 2,
                        pushed: 1,
                    },
                });
                self.output.push(AdapterType::String);
            }

            Descriptor::ClampedU8
            | Descriptor::Function(_)
            | Descriptor::Closure(_)
            | Descriptor::Slice(_)
            | Descriptor::Result(_) => bail!(
                "unsupported Result type for returning from exported Rust function: {:?}",
                arg
            ),

            // canot be reached
            Descriptor::Generic { .. } => unreachable!(),
        }
        Ok(())
    }

    fn outgoing_option_ref(&mut self, _mutable: bool, arg: &Descriptor) -> Result<(), Error> {
        match arg {
            Descriptor::Externref => {
                // If this is `Some` then it's the index, otherwise if it's
                // `None` then it's the index pointing to undefined.
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::TableGet,
                    &[AdapterType::Externref.option()],
                );
            }
            Descriptor::NamedExternref(name) => {
                self.instruction(
                    &[AdapterType::I32],
                    Instruction::TableGet,
                    &[AdapterType::NamedExternref(name.clone()).option()],
                );
            }
            Descriptor::CachedString => self.cached_string(false)?,
            Descriptor::String | Descriptor::Slice(_) => {
                let kind = arg.vector_kind().ok_or_else(|| {
                    format_err!(
                        "unsupported optional slice type for calling JS function from Rust {:?}",
                        arg
                    )
                })?;
                let mem = self.cx.memory()?;
                self.instruction(
                    &[AdapterType::I32, AdapterType::I32],
                    Instruction::OptionView {
                        kind: kind.clone(),
                        mem,
                    },
                    &[AdapterType::Vector(kind).option()],
                );
            }
            _ => bail!(
                "unsupported optional ref argument type for calling JS function from Rust: {:?}",
                arg
            ),
        }
        Ok(())
    }

    fn outgoing_string_enum(&mut self, name: &str) {
        self.instruction(
            &[AdapterType::I32],
            Instruction::WasmToStringEnum {
                name: name.to_string(),
            },
            &[AdapterType::StringEnum(name.to_string())],
        );
    }

    fn outgoing_i32(&mut self, output: AdapterType) {
        let instr = Instruction::WasmToInt32 {
            unsigned_32: output == AdapterType::U32 || output == AdapterType::NonNull,
        };
        self.instruction(&[AdapterType::I32], instr, &[output]);
    }
    fn outgoing_i64(&mut self, output: AdapterType) {
        let instr = Instruction::WasmToInt64 {
            unsigned: output == AdapterType::U64,
        };
        self.instruction(&[AdapterType::I64], instr, &[output]);
    }

    fn cached_string(&mut self, owned: bool) -> Result<(), Error> {
        let mem = self.cx.memory()?;
        let free = self.cx.free()?;
        self.instruction(
            &[AdapterType::I32, AdapterType::I32],
            Instruction::CachedStringLoad {
                owned,
                mem,
                free,
                table: None,
            },
            &[AdapterType::String],
        );
        Ok(())
    }

    fn option_native(&mut self, signed: bool, ty: ValType) {
        let adapter_ty = AdapterType::from_wasm(ty).unwrap();
        self.instruction(
            &[AdapterType::I32, adapter_ty.clone()],
            Instruction::ToOptionNative { signed, ty },
            &[adapter_ty.option()],
        );
    }

    fn out_option_sentinel32(&mut self, ty: AdapterType) {
        self.instruction(
            &[AdapterType::I32],
            Instruction::OptionU32Sentinel,
            &[ty.option()],
        );
    }

    fn out_option_sentinel64(&mut self, ty: AdapterType) {
        self.instruction(
            &[AdapterType::F64],
            Instruction::OptionF64Sentinel,
            &[ty.option()],
        );
    }
}

/// Converts a Descriptor to an AdapterType with handling generics/nested types
pub fn describe_map_to_adapter(arg: &Descriptor) -> Option<AdapterType> {
    match arg {
        Descriptor::Unit => Some(AdapterType::Unit),
        Descriptor::NonNull => Some(AdapterType::NonNull),

        Descriptor::I8 => Some(AdapterType::S8),
        Descriptor::U8 => Some(AdapterType::U8),
        Descriptor::I16 => Some(AdapterType::S16),
        Descriptor::U16 => Some(AdapterType::U16),
        Descriptor::I32 => Some(AdapterType::S32),
        Descriptor::U32 => Some(AdapterType::U32),
        Descriptor::I64 => Some(AdapterType::I64),
        Descriptor::U64 => Some(AdapterType::U64),
        Descriptor::I128 => Some(AdapterType::S128),
        Descriptor::U128 => Some(AdapterType::U128),
        Descriptor::F32 => Some(AdapterType::F32),
        Descriptor::F64 => Some(AdapterType::F64),
        Descriptor::Boolean => Some(AdapterType::Bool),
        Descriptor::Char | Descriptor::CachedString | Descriptor::String => {
            Some(AdapterType::String)
        }

        Descriptor::Enum { name, .. } => Some(AdapterType::Enum(name.clone())),
        Descriptor::StringEnum { name, .. } => Some(AdapterType::StringEnum(name.clone())),
        Descriptor::RustStruct(class) => Some(AdapterType::Struct(class.clone())),

        Descriptor::Externref => Some(AdapterType::Externref),
        Descriptor::NamedExternref(name) => Some(AdapterType::NamedExternref(name.clone())),

        Descriptor::Ref(d) | Descriptor::RefMut(d) | Descriptor::Result(d) => {
            describe_map_to_adapter(d)
        }

        Descriptor::Option(d) => Some(describe_map_to_adapter(d)?.option()),
        Descriptor::Vector(_) => {
            let kind = arg.vector_kind()?;
            Some(AdapterType::Vector(kind))
        }

        // Largely synthetic and can't show up
        Descriptor::ClampedU8 => unreachable!(),

        Descriptor::Function(_) | Descriptor::Closure(_) | Descriptor::Slice(_) => None,

        Descriptor::Generic { ty, args } => match &**ty {
            Descriptor::Result(d) => {
                if let Some(generics) = resolve_inner(args) {
                    Some(AdapterType::Generic {
                        ty: Box::new(describe_map_to_adapter(d)?),
                        args: generics.into_boxed_slice(),
                    })
                } else {
                    describe_map_to_adapter(d)
                }
            }
            Descriptor::Option(d) => {
                if let Some(generics) = resolve_inner(args) {
                    Some(AdapterType::Generic {
                        ty: Box::new(describe_map_to_adapter(d)?.option()),
                        args: generics.into_boxed_slice(),
                    })
                } else {
                    Some(describe_map_to_adapter(d)?.option())
                }
            }
            Descriptor::Vector(_d) => {
                let kind = ty.vector_kind()?;
                if let Some(generics) = resolve_inner(args) {
                    Some(AdapterType::Generic {
                        ty: Box::new(AdapterType::Vector(kind)),
                        args: generics.into_boxed_slice(),
                    })
                } else {
                    Some(AdapterType::Vector(kind))
                }
            }
            _ => {
                let mut generics = vec![];
                for arg in args {
                    generics.push(describe_map_to_adapter(arg)?);
                }
                if generics.is_empty() {
                    describe_map_to_adapter(ty)
                } else {
                    Some(AdapterType::Generic {
                        ty: Box::new(describe_map_to_adapter(ty)?),
                        args: generics.into_boxed_slice(),
                    })
                }
            }
        },
    }
}

/// Handles the generic argument of rust Result, Option and Vectors types.
/// This is because these types are generic but are encoded differently throughout
/// this repo compared to other generic types
pub fn resolve_inner(args: &[Descriptor]) -> Option<Vec<AdapterType>> {
    if let Some(Descriptor::Generic { ty, args, .. }) = args.first() {
        // vectors, results and options are already on level flattened, so we
        // need to check one level deeper for their possible generics
        match &**ty {
            Descriptor::Vector(_) | Descriptor::Result(_) | Descriptor::Option(_) => {
                resolve_inner(args)
            }
            _ => {
                let mut generic_args = vec![];
                for arg in args {
                    generic_args.push(describe_map_to_adapter(arg)?);
                }
                Some(generic_args)
            }
        }
    } else {
        None
    }
}
