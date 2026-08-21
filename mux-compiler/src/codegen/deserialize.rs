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

/// How a CSV cell becomes a declared type: the runtime parser to call and the
/// word for the error message. `None` means the field is already a string.
type CellCoercion = Option<(&'static str, &'static str)>;

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
    /// Another class, read through its own `from_value`.
    Nested(String),
    /// A JSON array, each element read the same way.
    ///
    /// `list<Json>` is how a HETEROGENEOUS array is declared - the element
    /// reader is `Raw`, so nothing is asserted about what each entry holds.
    Sequence(Box<FieldReader>),
}

impl<'a> CodeGenerator<'a> {
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
    pub(super) fn ensure_deserializer(&mut self, name: &str, method: &str) -> Result<(), String> {
        match method {
            "list_from_csv" => self.ensure_csv_deserializer(name),
            _ => self.ensure_json_deserializer(name),
        }
    }

    /// Emit the JSON side if it is not already emitted.
    ///
    /// Separate from the CSV side because the two support different field
    /// types: a CSV cell is text, so a nested class cannot come out of one.
    /// Emitting both together made `from_json` on a class with a nested field
    /// fail for a reason that belonged to a method the program never called.
    pub(super) fn ensure_json_deserializer(&mut self, name: &str) -> Result<(), String> {
        if self
            .module
            .get_function(&format!("{name}.from_json"))
            .is_some()
        {
            return Ok(());
        }
        let Some(fields) = self.classes.get(name).cloned() else {
            return Err(format!("no field list recorded for class '{name}'"));
        };
        self.while_paused(|emitter| {
            emitter
                .emit_from_value(name, &fields)
                .and_then(|()| emitter.emit_from_json(name))
                .and_then(|()| emitter.emit_list_from_json(name))
        })
    }

    fn ensure_csv_deserializer(&mut self, name: &str) -> Result<(), String> {
        if self
            .module
            .get_function(&format!("{name}.list_from_csv"))
            .is_some()
        {
            return Ok(());
        }
        let Some(fields) = self.classes.get(name).cloned() else {
            return Err(format!("no field list recorded for class '{name}'"));
        };
        self.while_paused(|emitter| {
            emitter
                .emit_from_row(name, &fields)
                .and_then(|()| emitter.emit_list_from_csv(name))
        })
    }

    /// Run `build` with the builder's position restored afterwards.
    ///
    /// Generation happens mid-expression, so it moves the insertion point; the
    /// same reason `ensure_enum_instantiated` saves and restores it.
    fn while_paused<F>(&mut self, build: F) -> Result<(), String>
    where
        F: FnOnce(&mut Self) -> Result<(), String>,
    {
        let resume = self.builder.get_insert_block();
        let result = build(self);
        if let Some(block) = resume {
            self.builder.position_at_end(block);
        }
        result
    }

    /// `<Class>!from_value(json)` - build an instance from an already-parsed
    /// value. This is the recursive core: a nested class field has a `Json` in
    /// hand, not text, so the part that reads fields cannot be the part that
    /// parses.
    ///
    /// The function is declared before its body is built, so a class that
    /// reaches itself through a field finds the declaration rather than
    /// recursing forever.
    fn emit_from_value(&mut self, name: &str, fields: &[Field]) -> Result<(), String> {
        let full_name = Self::from_value_name(name);
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let function = self.module.add_function(
            &full_name,
            ptr_type.fn_type(&[ptr_type.into()], false),
            None,
        );

        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        let document = function
            .get_nth_param(0)
            .ok_or("from_value takes the parsed document")?
            .into_pointer_value();

        let instance = self.call_returning_ptr(&format!("{}.new", name), &[])?;

        for field in fields {
            self.read_field_into(function, name, field, document, instance)?;
        }

        // `mux_result_ok_value` clones its argument rather than consuming it,
        // so the reference held here is still ours to release.
        let ok = self.call_returning_ptr("mux_result_ok_value", &[instance.into()])?;
        self.emit_value_decref(instance)?;
        self.builder
            .build_return(Some(&ok))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// `<Class>.from_json(text)` - parse, then hand the value to the core.
    fn emit_from_json(&mut self, name: &str) -> Result<(), String> {
        let full_name = format!("{}.from_json", name);
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let function = self.module.add_function(
            &full_name,
            ptr_type.fn_type(&[ptr_type.into()], false),
            None,
        );

        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        let text = function
            .get_nth_param(0)
            .ok_or("from_json takes the document text")?
            .into_pointer_value();

        // The parameter arrives as a Mux string, which is a reference-counted
        // `*mut Value`; `mux_json_parse` reads a C string. Extracting yields an
        // owned buffer that has to be freed after the parse.
        let cstr = self.extract_c_string_from_value(text)?;
        let parsed = self.call_returning_ptr("mux_json_parse", &[cstr.into()])?;
        let free = self
            .runtime_function("mux_free_string")
            .ok_or("mux_free_string not found")?;
        self.builder
            .build_call(free, &[cstr.into()], "free_json_text")
            .map_err(|e| e.to_string())?;

        let document = self.unwrap_result_or_return(function, parsed, "parse")?;
        let built = self.call_returning_ptr(&Self::from_value_name(name), &[document.into()])?;
        self.emit_value_decref(document)?;
        self.builder
            .build_return(Some(&built))
            .map_err(|e| e.to_string())?;

        self.constructors.insert(full_name, function);
        Ok(())
    }

    /// `<Class>.list_from_json(text)` - a JSON array of objects.
    ///
    /// Named for what it returns. `from_json` gives one instance and this gives
    /// many, so the caller can see which they are getting without checking the
    /// signature.
    /// Build `result<list<Class>, string>` by running `builder` over every entry
    /// of `items`, which is a `Value` holding a list.
    ///
    /// The JSON and CSV list forms differ only in how they get to this point -
    /// one parses a document and takes its array, the other pairs CSV rows with
    /// headers - so the loop itself is written once.
    ///
    /// `owned` is what the caller allocated to reach `items`, released on every
    /// exit. One failing entry fails the whole call and carries its own message:
    /// a partial list would be data the document does not contain.
    fn emit_collect_into_list(
        &mut self,
        function: FunctionValue<'a>,
        label: &str,
        items: PointerValue<'a>,
        builder: &str,
        owned: &[PointerValue<'a>],
    ) -> Result<(), String> {
        let count = self
            .call_returning_value("mux_value_list_length", &[items.into()])?
            .into_int_value();
        let out = self.call_returning_ptr("mux_new_list", &[])?;

        let i64_type = self.context.i64_type();
        let index = self
            .builder
            .build_alloca(i64_type, &format!("{label}_index"))
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(index, i64_type.const_zero())
            .map_err(|e| e.to_string())?;

        let head = self
            .context
            .append_basic_block(function, &format!("{label}_head"));
        let body = self
            .context
            .append_basic_block(function, &format!("{label}_body"));
        let done = self
            .context
            .append_basic_block(function, &format!("{label}_done"));
        self.builder
            .build_unconditional_branch(head)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(head);
        let current = self
            .builder
            .build_load(i64_type, index, "i")
            .map_err(|e| e.to_string())?
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, current, count, "more")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_conditional_branch(more, body, done)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(body);
        let entry =
            self.call_returning_ptr("mux_value_list_get_value", &[items.into(), current.into()])?;
        let built = self.call_returning_ptr(builder, &[entry.into()])?;
        self.emit_value_decref(entry)?;

        let built_ok = self
            .call_returning_value("mux_result_is_ok", &[built.into()])?
            .into_int_value();
        let kept = self
            .context
            .append_basic_block(function, &format!("{label}_entry_ok"));
        let failed = self
            .context
            .append_basic_block(function, &format!("{label}_entry_failed"));
        self.builder
            .build_conditional_branch(built_ok, kept, failed)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(failed);
        self.emit_list_free(out)?;
        for owned_ptr in owned {
            self.emit_value_decref(*owned_ptr)?;
        }
        self.builder
            .build_return(Some(&built))
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(kept);
        let object = self.call_returning_ptr("mux_result_data", &[built.into()])?;
        self.emit_value_decref(built)?;
        // `push_back` clones what it is given.
        self.call_returning_value("mux_list_push_back", &[out.into(), object.into()])
            .ok();
        self.emit_value_decref(object)?;

        let next = self
            .builder
            .build_int_add(current, i64_type.const_int(1, false), "next")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(index, next)
            .map_err(|e| e.to_string())?;
        self.builder
            .build_unconditional_branch(head)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(done);
        let list_value = self.call_returning_ptr("mux_list_value", &[out.into()])?;
        for owned_ptr in owned {
            self.emit_value_decref(*owned_ptr)?;
        }
        let ok = self.call_returning_ptr("mux_result_ok_value", &[list_value.into()])?;
        self.emit_value_decref(list_value)?;
        self.builder
            .build_return(Some(&ok))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn emit_list_from_json(&mut self, name: &str) -> Result<(), String> {
        let full_name = format!("{}.list_from_json", name);
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let function = self.module.add_function(
            &full_name,
            ptr_type.fn_type(&[ptr_type.into()], false),
            None,
        );

        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        let text = function
            .get_nth_param(0)
            .ok_or("list_from_json takes the document text")?
            .into_pointer_value();

        let cstr = self.extract_c_string_from_value(text)?;
        let parsed = self.call_returning_ptr("mux_json_parse", &[cstr.into()])?;
        let free = self
            .runtime_function("mux_free_string")
            .ok_or("mux_free_string not found")?;
        self.builder
            .build_call(free, &[cstr.into()], "free_json_text")
            .map_err(|e| e.to_string())?;
        let document = self.unwrap_result_or_return(function, parsed, "parse")?;

        // A document that parsed but is not an array is the mistake worth
        // naming: `from_json` was probably meant.
        let as_list = self.call_returning_ptr("mux_json_as_list", &[document.into()])?;
        let is_array = self
            .call_returning_value("mux_result_is_ok", &[as_list.into()])?
            .into_int_value();
        let array_block = self.context.append_basic_block(function, "is_array");
        let not_array = self.context.append_basic_block(function, "not_array");
        self.builder
            .build_conditional_branch(is_array, array_block, not_array)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(not_array);
        self.emit_value_decref(as_list)?;
        self.emit_value_decref(document)?;
        self.return_error("expected a JSON array; use from_json for a single object")?;

        self.builder.position_at_end(array_block);
        let items = self.call_returning_ptr("mux_result_data", &[as_list.into()])?;
        self.emit_value_decref(as_list)?;
        self.emit_collect_into_list(
            function,
            "element",
            items,
            &Self::from_value_name(name),
            &[items, document],
        )?;

        self.constructors.insert(full_name, function);
        Ok(())
    }

    /// `<Class>!from_row(map)` - build an instance from one CSV row.
    ///
    /// Separate from `from_value` because CSV has no types: every cell arrives
    /// as text, so an `int` field must PARSE `"9"` rather than reject it as the
    /// wrong kind. Same field lookup, different conversion.
    fn emit_from_row(&mut self, name: &str, fields: &[Field]) -> Result<(), String> {
        let full_name = Self::from_row_name(name);
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let function = self.module.add_function(
            &full_name,
            ptr_type.fn_type(&[ptr_type.into()], false),
            None,
        );

        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        let row = function
            .get_nth_param(0)
            .ok_or("from_row takes the row map")?
            .into_pointer_value();

        let instance = self.call_returning_ptr(&format!("{}.new", name), &[])?;

        for field in fields {
            self.read_cell_into(function, name, field, row, instance)?;
        }

        let ok = self.call_returning_ptr("mux_result_ok_value", &[instance.into()])?;
        self.emit_value_decref(instance)?;
        self.builder
            .build_return(Some(&ok))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// One column of one row, coerced to the declared field type.
    fn read_cell_into(
        &mut self,
        function: FunctionValue<'a>,
        class_name: &str,
        field: &Field,
        row: PointerValue<'a>,
        instance: PointerValue<'a>,
    ) -> Result<(), String> {
        let (coercion, optional) = Self::cell_coercion(class_name, field)?;

        let key = self.build_global_cstring(&field.name)?;
        let found = self.call_returning_ptr("mux_json_field", &[row.into(), key.into()])?;
        let present = self
            .call_returning_value("mux_optional_is_some", &[found.into()])?
            .into_int_value();

        let present_block = self
            .context
            .append_basic_block(function, &format!("{}_cell", field.name));
        let absent_block = self
            .context
            .append_basic_block(function, &format!("{}_no_cell", field.name));
        let done_block = self
            .context
            .append_basic_block(function, &format!("{}_cell_done", field.name));
        self.builder
            .build_conditional_branch(present, present_block, absent_block)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(absent_block);
        self.emit_value_decref(found)?;
        if optional {
            self.builder
                .build_unconditional_branch(done_block)
                .map_err(|e| e.to_string())?;
        } else {
            self.emit_value_decref(instance)?;
            self.return_error(&format!("missing required column '{}'", field.name))?;
        }

        self.builder.position_at_end(present_block);
        let cell = self.call_returning_ptr("mux_optional_get_value", &[found.into()])?;
        self.emit_value_decref(found)?;

        let value = match coercion {
            // A string column is already what the field wants.
            None => cell,
            Some((runtime_fn, expected)) => {
                let text = self.extract_c_string_from_value(cell)?;
                self.emit_value_decref(cell)?;
                let parsed = self.call_returning_ptr(runtime_fn, &[text.into()])?;
                let free = self
                    .runtime_function("mux_free_string")
                    .ok_or("mux_free_string not found")?;
                self.builder
                    .build_call(free, &[text.into()], "free_cell")
                    .map_err(|e| e.to_string())?;

                let parsed_ok = self
                    .call_returning_value("mux_result_is_ok", &[parsed.into()])?
                    .into_int_value();
                let good = self
                    .context
                    .append_basic_block(function, &format!("{}_cell_ok", field.name));
                let bad = self
                    .context
                    .append_basic_block(function, &format!("{}_cell_bad", field.name));
                self.builder
                    .build_conditional_branch(parsed_ok, good, bad)
                    .map_err(|e| e.to_string())?;

                self.builder.position_at_end(bad);
                self.emit_value_decref(parsed)?;
                self.emit_value_decref(instance)?;
                self.return_error(&format!("column '{}': expected {expected}", field.name))?;

                self.builder.position_at_end(good);
                let inner = self.call_returning_ptr("mux_result_data", &[parsed.into()])?;
                self.emit_value_decref(parsed)?;
                inner
            }
        };

        let stored = if optional {
            let wrapped = self.call_returning_ptr("mux_optional_some_value", &[value.into()])?;
            self.emit_value_decref(value)?;
            wrapped
        } else {
            value
        };

        self.store_deserialized_field(class_name, field, instance, stored)?;
        self.builder
            .build_unconditional_branch(done_block)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(done_block);
        Ok(())
    }

    /// How a CSV cell becomes the declared type: the parser to call and the
    /// word for the error, or `None` when the field is already a string.
    fn cell_coercion(class_name: &str, field: &Field) -> Result<(CellCoercion, bool), String> {
        let (inner, optional) = match &field.type_.kind {
            TypeKind::Named(name, args) if name == "optional" && args.len() == 1 => {
                (&args[0].kind, true)
            }
            other => (other, false),
        };

        let coercion = match inner {
            TypeKind::Primitive(PrimitiveType::Str) => None,
            TypeKind::Primitive(PrimitiveType::Int) => Some(("mux_string_to_int", "an int")),
            TypeKind::Primitive(PrimitiveType::Float) => Some(("mux_string_to_float", "a float")),
            TypeKind::Primitive(PrimitiveType::Bool) => {
                Some(("mux_string_to_bool", "true or false"))
            }
            _ => {
                return Err(format!(
                    "'{class_name}.list_from_csv' cannot read column '{}': a CSV cell is text, so \
                     only string, int, float, bool and their optionals can be read from one",
                    field.name
                ));
            }
        };
        Ok((coercion, optional))
    }

    /// `<Class>.list_from_csv(text)` - the rows of a CSV table.
    ///
    /// There is no singular `from_csv`: a CSV document IS a table, so a
    /// singular form would only work for a file with exactly one row and would
    /// read as a promise the format cannot keep.
    fn emit_list_from_csv(&mut self, name: &str) -> Result<(), String> {
        let full_name = format!("{}.list_from_csv", name);
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let function = self.module.add_function(
            &full_name,
            ptr_type.fn_type(&[ptr_type.into()], false),
            None,
        );

        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        let text = function
            .get_nth_param(0)
            .ok_or("list_from_csv takes the document text")?
            .into_pointer_value();

        let cstr = self.extract_c_string_from_value(text)?;
        let parsed = self.call_returning_ptr("mux_csv_parse_with_headers", &[cstr.into()])?;
        let free = self
            .runtime_function("mux_free_string")
            .ok_or("mux_free_string not found")?;
        self.builder
            .build_call(free, &[cstr.into()], "free_csv_text")
            .map_err(|e| e.to_string())?;
        let table = self.unwrap_result_or_return(function, parsed, "csv")?;

        let as_rows = self.call_returning_ptr("mux_csv_rows_as_maps", &[table.into()])?;
        let paired = self
            .call_returning_value("mux_result_is_ok", &[as_rows.into()])?
            .into_int_value();
        let rows_block = self.context.append_basic_block(function, "has_rows");
        let no_rows = self.context.append_basic_block(function, "no_rows");
        self.builder
            .build_conditional_branch(paired, rows_block, no_rows)
            .map_err(|e| e.to_string())?;

        // Pairing says why it could not - a duplicate column names itself -
        // so its message is returned rather than replaced with a vaguer one.
        self.builder.position_at_end(no_rows);
        self.emit_value_decref(table)?;
        self.builder
            .build_return(Some(&as_rows))
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(rows_block);
        let rows = self.call_returning_ptr("mux_result_data", &[as_rows.into()])?;
        self.emit_value_decref(as_rows)?;
        self.emit_collect_into_list(
            function,
            "row",
            rows,
            &Self::from_row_name(name),
            &[rows, table],
        )?;

        self.constructors.insert(full_name, function);
        Ok(())
    }

    /// Free a raw `List` handle from `mux_new_list`.
    ///
    /// Not reference counted, so `mux_rc_dec` does not reach it and
    /// `rc-leak-check` cannot see it leak - only valgrind does. An early return
    /// that abandons a half-built list has to free it explicitly.
    fn emit_list_free(&mut self, list: PointerValue<'a>) -> Result<(), String> {
        let free = self
            .runtime_function("mux_free_list")
            .ok_or("mux_free_list not found")?;
        self.builder
            .build_call(free, &[list.into()], "free_partial_list")
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn from_row_name(class_name: &str) -> String {
        format!("{class_name}!from_row")
    }

    fn from_value_name(class_name: &str) -> String {
        format!("{class_name}!from_value")
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
            self.return_error(&format!("missing required field '{}'", field.name))?;
        }

        self.builder.position_at_end(present_block);
        let raw = self.call_returning_ptr("mux_optional_get_value", &[found.into()])?;
        self.emit_value_decref(found)?;

        // An `optional<T>` field accepts an explicit `null` as well as absence,
        // and both mean `none`. `new` already left `none` in the slot, so the
        // null case joins the absent path rather than converting a value that is
        // not there.
        if optional {
            let is_null = self
                .call_returning_value("mux_json_is_null", &[raw.into()])?
                .into_int_value();
            let null_block = self
                .context
                .append_basic_block(function, &format!("{}_null", field.name));
            let value_block = self
                .context
                .append_basic_block(function, &format!("{}_value", field.name));
            self.builder
                .build_conditional_branch(is_null, null_block, value_block)
                .map_err(|e| e.to_string())?;

            self.builder.position_at_end(null_block);
            self.emit_value_decref(raw)?;
            self.builder
                .build_unconditional_branch(done_block)
                .map_err(|e| e.to_string())?;

            self.builder.position_at_end(value_block);
        }

        let converted = self.convert_json_value(function, &field.name, &reader, raw, instance)?;

        // The slot holds the optional itself, so a present value is wrapped
        // back up. `mux_optional_some_value` clones, so the inner value is
        // still ours to release.
        let value = if optional {
            let wrapped =
                self.call_returning_ptr("mux_optional_some_value", &[converted.into()])?;
            self.emit_value_decref(converted)?;
            wrapped
        } else {
            converted
        };

        self.store_deserialized_field(class_name, field, instance, value)?;
        self.builder
            .build_unconditional_branch(done_block)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(done_block);
        Ok(())
    }

    /// Which accessor reads this field, and whether it may be absent.
    /// Convert one JSON value to the representation its declared type wants.
    ///
    /// Recursive because a declared type can nest: `list<Item>` reads each
    /// element through `Item`'s own deserializer, and `list<Json>` reads each
    /// element as nothing at all, which is what makes a heterogeneous array
    /// expressible.
    ///
    /// Any failure returns from the enclosing function, releasing `instance` -
    /// the partially built object is not something a caller should ever see.
    fn convert_json_value(
        &mut self,
        function: FunctionValue<'a>,
        label: &str,
        reader: &FieldReader,
        raw: PointerValue<'a>,
        instance: PointerValue<'a>,
    ) -> Result<PointerValue<'a>, String> {
        match reader {
            FieldReader::Raw => Ok(raw),
            FieldReader::Accessor {
                runtime_fn,
                expected,
            } => self.convert_via_accessor(function, label, runtime_fn, expected, raw, instance),
            FieldReader::Nested(nested) => {
                self.convert_via_nested(function, label, nested, raw, instance)
            }
            FieldReader::Sequence(element) => {
                self.convert_sequence(function, label, element, raw, instance)
            }
        }
    }

    /// A JSON array, each element converted by `element`.
    fn convert_sequence(
        &mut self,
        function: FunctionValue<'a>,
        label: &str,
        element: &FieldReader,
        raw: PointerValue<'a>,
        instance: PointerValue<'a>,
    ) -> Result<PointerValue<'a>, String> {
        let as_list = self.call_returning_ptr("mux_json_as_list", &[raw.into()])?;
        self.emit_value_decref(raw)?;
        let is_array = self
            .call_returning_value("mux_result_is_ok", &[as_list.into()])?
            .into_int_value();
        let ok_block = self
            .context
            .append_basic_block(function, &format!("{label}_is_array"));
        let bad = self
            .context
            .append_basic_block(function, &format!("{label}_not_array"));
        self.builder
            .build_conditional_branch(is_array, ok_block, bad)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(bad);
        self.emit_value_decref(as_list)?;
        self.emit_value_decref(instance)?;
        self.return_error(&format!("field '{label}': expected an array"))?;

        self.builder.position_at_end(ok_block);
        let items = self.call_returning_ptr("mux_result_data", &[as_list.into()])?;
        self.emit_value_decref(as_list)?;
        let count = self
            .call_returning_value("mux_value_list_length", &[items.into()])?
            .into_int_value();
        let out = self.call_returning_ptr("mux_new_list", &[])?;

        let i64_type = self.context.i64_type();
        let index = self
            .builder
            .build_alloca(i64_type, &format!("{label}_i"))
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(index, i64_type.const_zero())
            .map_err(|e| e.to_string())?;

        let head = self
            .context
            .append_basic_block(function, &format!("{label}_head"));
        let body = self
            .context
            .append_basic_block(function, &format!("{label}_body"));
        let done = self
            .context
            .append_basic_block(function, &format!("{label}_end"));
        self.builder
            .build_unconditional_branch(head)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(head);
        let current = self
            .builder
            .build_load(i64_type, index, "i")
            .map_err(|e| e.to_string())?
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, current, count, "more")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_conditional_branch(more, body, done)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(body);
        let entry =
            self.call_returning_ptr("mux_value_list_get_value", &[items.into(), current.into()])?;
        let converted = self.convert_json_value(function, label, element, entry, instance)?;
        // `push_back` clones, so the converted element is still ours.
        self.call_returning_value("mux_list_push_back", &[out.into(), converted.into()])
            .ok();
        self.emit_value_decref(converted)?;

        let next = self
            .builder
            .build_int_add(current, i64_type.const_int(1, false), "next")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(index, next)
            .map_err(|e| e.to_string())?;
        self.builder
            .build_unconditional_branch(head)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(done);
        self.emit_value_decref(items)?;
        self.call_returning_ptr("mux_list_value", &[out.into()])
    }

    fn convert_via_accessor(
        &mut self,
        function: FunctionValue<'a>,
        label: &str,
        runtime_fn: &str,
        expected: &str,
        raw: PointerValue<'a>,
        instance: PointerValue<'a>,
    ) -> Result<PointerValue<'a>, String> {
        let converted = self.call_returning_ptr(runtime_fn, &[raw.into()])?;
        self.emit_value_decref(raw)?;
        let right_kind = self
            .call_returning_value("mux_result_is_ok", &[converted.into()])?
            .into_int_value();

        let good = self
            .context
            .append_basic_block(function, &format!("{label}_kind_ok"));
        let bad = self
            .context
            .append_basic_block(function, &format!("{label}_kind_bad"));
        self.builder
            .build_conditional_branch(right_kind, good, bad)
            .map_err(|e| e.to_string())?;

        // The accessor's own message says what was there; this prefixes the
        // field so the reader learns both, rather than one at the cost of the
        // other.
        self.builder.position_at_end(bad);
        self.emit_value_decref(converted)?;
        self.emit_value_decref(instance)?;
        self.return_error(&format!("field '{label}': expected {expected}"))?;

        self.builder.position_at_end(good);
        let inner = self.call_returning_ptr("mux_result_data", &[converted.into()])?;
        self.emit_value_decref(converted)?;
        Ok(inner)
    }

    fn convert_via_nested(
        &mut self,
        function: FunctionValue<'a>,
        label: &str,
        nested: &str,
        raw: PointerValue<'a>,
        instance: PointerValue<'a>,
    ) -> Result<PointerValue<'a>, String> {
        self.ensure_json_deserializer(nested)?;
        let built = self.call_returning_ptr(&Self::from_value_name(nested), &[raw.into()])?;
        self.emit_value_decref(raw)?;

        // The nested build can fail on its own fields. Its message already names
        // the field that was wrong, so it is returned unchanged rather than
        // wrapped in one that says less.
        let nested_ok = self
            .call_returning_value("mux_result_is_ok", &[built.into()])?
            .into_int_value();
        let good = self
            .context
            .append_basic_block(function, &format!("{label}_nested_ok"));
        let bad = self
            .context
            .append_basic_block(function, &format!("{label}_nested_bad"));
        self.builder
            .build_conditional_branch(nested_ok, good, bad)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(bad);
        self.emit_value_decref(instance)?;
        self.builder
            .build_return(Some(&built))
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(good);
        let inner = self.call_returning_ptr("mux_result_data", &[built.into()])?;
        self.emit_value_decref(built)?;
        Ok(inner)
    }

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
            TypeKind::List(element) => {
                let element_field = Field {
                    name: field.name.clone(),
                    type_: (**element).clone(),
                    is_generic_param: false,
                    is_const: false,
                    default_value: None,
                    where_clause: None,
                };
                let (element_reader, _) = Self::field_reader(class_name, &element_field)?;
                FieldReader::Sequence(Box::new(element_reader))
            }
            // Any other named type with no arguments is taken to be a class.
            // If it is not one, `ensure_deserializer` fails with the class name,
            // which is more use than a generic "unsupported field".
            TypeKind::Named(name, args) if args.is_empty() => FieldReader::Nested(name.clone()),
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
