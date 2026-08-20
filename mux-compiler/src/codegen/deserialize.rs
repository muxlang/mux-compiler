//! Building a class instance from a parsed document.
//!
//! `Config.from_json(text)` is synthesized for every class (see
//! `semantics::declarations`), and this emits its body. The shape is the same
//! for every class, so it is generated rather than written:
//!
//! ```text
//! parse the text            -> result<Json, string>, propagated on failure
//! Config.new()              -> a zeroed instance, vtables and invariants done
//! for each declared field:
//!     look the field up     -> optional<Json>
//!     absent?               -> `none` if the field is optional, else an error
//!     convert to the type   -> optional<T>, an error if it is the wrong kind
//!     store it
//! wrap in ok
//! ```
//!
//! Reusing `Config.new()` for the allocation is deliberate: it already
//! registers the type, sets vtable fields and runs construction invariants, and
//! duplicating that here would mean two places to keep in step.

use super::CodeGenerator;
use crate::ast::{Field, PrimitiveType, TypeKind};
use crate::semantics::Type;
use inkwell::AddressSpace;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};

/// How a declared field is read out of a JSON value.
enum FieldReader {
    /// A primitive with a `mux_json_as_*` accessor.
    Accessor {
        runtime_fn: &'static str,
        /// Named in the error when the field holds a different kind.
        expected: &'static str,
    },
    /// The field keeps the `Json` as-is - the escape hatch for parts of a
    /// document whose shape is not known, including heterogeneous arrays.
    Raw,
}

impl<'a> CodeGenerator<'a> {
    /// Emit `<Class>.from_json`.
    ///
    /// Only the flat cases are handled so far: primitives, and `Json` fields
    /// kept as-is. Nested classes and lists are rejected with a diagnostic
    /// rather than silently mis-built, so a program either works or is told
    /// exactly which field is not supported yet.
    /// Emit `<Class>.from_json` if it has not been emitted already.
    ///
    /// On demand rather than for every class, because a class whose fields this
    /// cannot read yet is perfectly legal as long as nothing asks it to
    /// deserialize. Generating eagerly turned an unsupported FIELD TYPE into a
    /// compile error for programs that never mention `from_json` - a class with
    /// a `char` field stopped building.
    ///
    /// This runs mid-expression, so it saves and restores the builder's
    /// insertion point, the same way `ensure_enum_instantiated` does.
    pub(super) fn ensure_deserializer(&mut self, name: &str) -> Result<(), String> {
        let full_name = format!("{}.from_json", name);
        if self.module.get_function(&full_name).is_some() {
            return Ok(());
        }
        let Some(fields) = self.classes.get(name).cloned() else {
            return Err(format!("no field list recorded for class '{name}'"));
        };

        let resume = self.builder.get_insert_block();
        let result = self.generate_class_deserializers(name, &fields);
        if let Some(block) = resume {
            self.builder.position_at_end(block);
        }
        result
    }

    fn generate_class_deserializers(&mut self, name: &str, fields: &[Field]) -> Result<(), String> {
        let full_name = format!("{}.from_json", name);
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let fn_type = ptr_type.fn_type(&[ptr_type.into()], false);
        let function = self.module.add_function(&full_name, fn_type, None);

        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        let text = function
            .get_nth_param(0)
            .ok_or("from_json takes the document text")?
            .into_pointer_value();

        // The parameter arrives as a Mux string, which is a reference-counted
        // `*mut Value`; `mux_json_parse` reads a C string. Extracting yields an
        // owned buffer that has to be freed after the parse - the same
        // convention every other `*_to_string` caller follows.
        let cstr = self.extract_c_string_from_value(text)?;
        let parsed = self.call_returning_ptr("mux_json_parse", &[cstr.into()])?;
        let free = self
            .runtime_function("mux_free_string")
            .ok_or("mux_free_string not found")?;
        self.builder
            .build_call(free, &[cstr.into()], "free_json_text")
            .map_err(|e| e.to_string())?;
        let document = self.unwrap_result_or_return(function, parsed, "parse")?;

        let instance = self.call_returning_ptr(&format!("{}.new", name), &[])?;

        for field in fields {
            self.read_field_into(function, name, field, document, instance)?;
        }

        self.emit_value_decref(document)?;
        // `mux_result_ok_value` clones its argument rather than consuming it,
        // so the reference held here is still ours to release.
        let ok = self.call_returning_ptr("mux_result_ok_value", &[instance.into()])?;
        self.emit_value_decref(instance)?;
        self.builder
            .build_return(Some(&ok))
            .map_err(|e| e.to_string())?;

        self.constructors.insert(full_name, function);
        Ok(())
    }

    /// Take the `ok` payload out of a `result`, returning the whole result
    /// unchanged when it is an `err`.
    ///
    /// Propagating the original rather than building a new error keeps the
    /// parser's message - "expected value at line 3" is worth more than
    /// "could not parse".
    fn unwrap_result_or_return(
        &mut self,
        function: FunctionValue<'a>,
        result: PointerValue<'a>,
        label: &str,
    ) -> Result<PointerValue<'a>, String> {
        let is_ok = self
            .call_returning_value("mux_result_is_ok", &[result.into()])?
            .into_int_value();

        let ok_block = self
            .context
            .append_basic_block(function, &format!("{label}_ok"));
        let err_block = self
            .context
            .append_basic_block(function, &format!("{label}_err"));
        self.builder
            .build_conditional_branch(is_ok, ok_block, err_block)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(err_block);
        self.builder
            .build_return(Some(&result))
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(ok_block);
        // `mux_result_data` clones the payload out, so the result itself is
        // finished with here. The err block returns it instead, handing that
        // reference to the caller.
        let payload = self.call_returning_ptr("mux_result_data", &[result.into()])?;
        self.emit_value_decref(result)?;
        Ok(payload)
    }

    /// Emit the read for one declared field.
    fn read_field_into(
        &mut self,
        function: FunctionValue<'a>,
        class_name: &str,
        field: &Field,
        document: PointerValue<'a>,
        instance: PointerValue<'a>,
    ) -> Result<(), String> {
        let (reader, optional) = Self::field_reader(class_name, field)?;

        let key = self.build_global_cstring(&field.name)?;
        let found = self.call_returning_ptr("mux_json_field", &[document.into(), key.into()])?;
        let present = self
            .call_returning_value("mux_optional_is_some", &[found.into()])?
            .into_int_value();

        let present_block = self
            .context
            .append_basic_block(function, &format!("{}_present", field.name));
        let absent_block = self
            .context
            .append_basic_block(function, &format!("{}_absent", field.name));
        let done_block = self
            .context
            .append_basic_block(function, &format!("{}_done", field.name));

        self.builder
            .build_conditional_branch(present, present_block, absent_block)
            .map_err(|e| e.to_string())?;

        // Absent. An optional field is already `none` from `new`, so there is
        // nothing to store; a required one is an error naming the field, which
        // is the only thing the caller can act on.
        self.builder.position_at_end(absent_block);
        self.emit_value_decref(found)?;
        if optional {
            self.builder
                .build_unconditional_branch(done_block)
                .map_err(|e| e.to_string())?;
        } else {
            self.emit_value_decref(instance)?;
            self.emit_value_decref(document)?;
            self.return_error(&format!("missing required field '{}'", field.name))?;
        }

        self.builder.position_at_end(present_block);
        let raw = self.call_returning_ptr("mux_optional_get_value", &[found.into()])?;
        self.emit_value_decref(found)?;
        let value = match reader {
            FieldReader::Raw => raw,
            FieldReader::Accessor {
                runtime_fn,
                expected,
            } => {
                let converted = self.call_returning_ptr(runtime_fn, &[raw.into()])?;
                self.emit_value_decref(raw)?;
                let right_kind = self
                    .call_returning_value("mux_optional_is_some", &[converted.into()])?
                    .into_int_value();

                let good = self
                    .context
                    .append_basic_block(function, &format!("{}_kind_ok", field.name));
                let bad = self
                    .context
                    .append_basic_block(function, &format!("{}_kind_bad", field.name));
                self.builder
                    .build_conditional_branch(right_kind, good, bad)
                    .map_err(|e| e.to_string())?;

                self.builder.position_at_end(bad);
                self.emit_value_decref(converted)?;
                self.emit_value_decref(instance)?;
                self.emit_value_decref(document)?;
                self.return_error(&format!("field '{}': expected {}", field.name, expected))?;

                self.builder.position_at_end(good);
                let inner =
                    self.call_returning_ptr("mux_optional_get_value", &[converted.into()])?;
                self.emit_value_decref(converted)?;
                inner
            }
        };

        self.store_deserialized_field(class_name, field, instance, value)?;
        self.builder
            .build_unconditional_branch(done_block)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(done_block);
        Ok(())
    }

    /// Which accessor reads this field, and whether it may be absent.
    fn field_reader(class_name: &str, field: &Field) -> Result<(FieldReader, bool), String> {
        let (inner, optional) = match &field.type_.kind {
            TypeKind::Named(name, args) if name == "optional" && args.len() == 1 => {
                (&args[0].kind, true)
            }
            other => (other, false),
        };

        let reader = match inner {
            TypeKind::Primitive(PrimitiveType::Int) => FieldReader::Accessor {
                runtime_fn: "mux_json_as_int",
                expected: "an int",
            },
            TypeKind::Primitive(PrimitiveType::Float) => FieldReader::Accessor {
                runtime_fn: "mux_json_as_float",
                expected: "a float",
            },
            TypeKind::Primitive(PrimitiveType::Bool) => FieldReader::Accessor {
                runtime_fn: "mux_json_as_bool",
                expected: "a bool",
            },
            TypeKind::Primitive(PrimitiveType::Str) => FieldReader::Accessor {
                runtime_fn: "mux_json_as_string",
                expected: "a string",
            },
            TypeKind::Named(name, args) if name == "Json" && args.is_empty() => FieldReader::Raw,
            _ => {
                return Err(format!(
                    "'{class_name}.from_json' cannot read field '{}': only int, float, bool, \
                     string, their optionals, and Json are supported so far",
                    field.name
                ));
            }
        };
        Ok((reader, optional))
    }

    /// Call a function that returns `*mut Value`, whether it is a runtime
    /// symbol or one this module emitted (`Config.new`).
    fn call_returning_ptr(
        &mut self,
        name: &str,
        args: &[inkwell::values::BasicMetadataValueEnum<'a>],
    ) -> Result<PointerValue<'a>, String> {
        Ok(self.call_returning_value(name, args)?.into_pointer_value())
    }

    fn call_returning_value(
        &mut self,
        name: &str,
        args: &[inkwell::values::BasicMetadataValueEnum<'a>],
    ) -> Result<BasicValueEnum<'a>, String> {
        let func = self
            .runtime_function(name)
            .or_else(|| self.module.get_function(name))
            .ok_or_else(|| format!("{name} not found"))?;
        self.builder
            .build_call(func, args, "call")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| format!("{name} should return a value"))
    }

    fn build_global_cstring(&mut self, text: &str) -> Result<PointerValue<'a>, String> {
        self.builder
            .build_global_string_ptr(text, &format!("json_key_{text}"))
            .map(|g| g.as_pointer_value())
            .map_err(|e| e.to_string())
    }

    /// Return `err(message)` from the current block.
    fn return_error(&mut self, message: &str) -> Result<(), String> {
        let text = self.build_global_cstring(message)?;
        let err = self.call_returning_ptr("mux_result_err_str", &[text.into()])?;
        self.builder
            .build_return(Some(&err))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Store a converted value into the instance's field slot.
    ///
    /// The slot's representation decides the write, not the value's type:
    /// `scalar_slot_type` says int, float and bool live inline, so those are
    /// unboxed and the intermediate released. A string field holds a pointer to
    /// a reference-counted value, so ownership of the extracted value transfers
    /// straight into the slot and must NOT be released here. An `optional<T>`
    /// field holds the optional itself, so the value is re-wrapped.
    fn store_deserialized_field(
        &mut self,
        class_name: &str,
        field: &Field,
        instance: PointerValue<'a>,
        value: PointerValue<'a>,
    ) -> Result<(), String> {
        let field_index = *self
            .field_map
            .get(class_name)
            .ok_or_else(|| format!("no field map for {class_name}"))?
            .get(&field.name)
            .ok_or_else(|| format!("no field '{}' on {class_name}", field.name))?;
        let class_type = *self
            .type_map
            .get(class_name)
            .ok_or_else(|| format!("no layout for {class_name}"))?;

        let data_ptr = self.call_returning_ptr("mux_get_object_ptr", &[instance.into()])?;
        let slot = self
            .builder
            .build_struct_gep(class_type, data_ptr, field_index as u32, &field.name)
            .map_err(|e| e.to_string())?;

        let field_type = self.resolve_field_semantic_type(field)?;
        let stored = match self.scalar_slot_type(&field_type) {
            Some(_) => {
                let unbox = match &field_type {
                    Type::Primitive(PrimitiveType::Float) => "mux_value_get_float",
                    Type::Primitive(PrimitiveType::Bool) => "mux_value_get_bool",
                    _ => "mux_value_get_int",
                };
                let scalar = self.call_returning_value(unbox, &[value.into()])?;
                self.emit_value_decref(value)?;
                scalar
            }
            // A pointer slot takes the value itself, and with it the reference
            // this function is holding - but whatever `new` put in the slot has
            // to be released first, or the default it wrote leaks.
            None => {
                let previous = self
                    .builder
                    .build_load(
                        self.context.ptr_type(AddressSpace::default()),
                        slot,
                        "previous",
                    )
                    .map_err(|e| e.to_string())?
                    .into_pointer_value();
                self.emit_value_decref(previous)?;
                value.into()
            }
        };

        self.builder
            .build_store(slot, stored)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The semantic type of a declared field, for deciding its slot shape.
    fn resolve_field_semantic_type(&mut self, field: &Field) -> Result<Type, String> {
        self.analyzer
            .resolve_type(&field.type_)
            .map_err(|e| format!("field '{}': {}", field.name, e.message))
    }
}
