use super::{MethodSig, SemanticAnalyzer, Type};
use crate::ast::PrimitiveType;

impl SemanticAnalyzer {
    fn get_primitive_method_sig(
        &self,
        prim: &PrimitiveType,
        method_name: &str,
    ) -> Option<MethodSig> {
        use PrimitiveType::{Bool, Byte, Bytes, Char, Float, Int, Str};
        let resolver = match prim {
            Int => Some(Self::get_int_method_sig as fn(&Self, &str) -> Option<MethodSig>),
            Byte => Some(Self::get_byte_method_sig as fn(&Self, &str) -> Option<MethodSig>),
            Float => Some(Self::get_float_method_sig as fn(&Self, &str) -> Option<MethodSig>),
            Str => Some(Self::get_string_method_sig as fn(&Self, &str) -> Option<MethodSig>),
            Bytes => Some(Self::get_bytes_method_sig as fn(&Self, &str) -> Option<MethodSig>),
            Bool => Some(Self::get_bool_method_sig as fn(&Self, &str) -> Option<MethodSig>),
            Char => Some(Self::get_char_method_sig as fn(&Self, &str) -> Option<MethodSig>),
            PrimitiveType::Void | PrimitiveType::Auto => None,
        };
        resolver.and_then(|resolve| resolve(self, method_name))
    }

    fn make_instance_method_sig(params: Vec<Type>, return_type: Type) -> MethodSig {
        MethodSig {
            params,
            return_type,
            is_static: false,
        }
    }

    fn make_eq_method_sig(param_type: PrimitiveType) -> MethodSig {
        Self::make_instance_method_sig(
            vec![Type::Primitive(param_type)],
            Type::Primitive(PrimitiveType::Bool),
        )
    }

    fn make_cmp_method_sig(param_type: PrimitiveType) -> MethodSig {
        Self::make_instance_method_sig(
            vec![Type::Primitive(param_type)],
            Type::Primitive(PrimitiveType::Int),
        )
    }

    fn make_hash_method_sig() -> MethodSig {
        Self::make_instance_method_sig(vec![], Type::Primitive(PrimitiveType::Int))
    }

    fn make_to_string_method_sig() -> MethodSig {
        Self::make_instance_method_sig(vec![], Type::Primitive(PrimitiveType::Str))
    }

    fn make_str_parse_result_method_sig(value_type: PrimitiveType) -> MethodSig {
        Self::make_instance_method_sig(
            vec![],
            Type::Result(
                Box::new(Type::Primitive(value_type)),
                Box::new(Type::Primitive(PrimitiveType::Str)),
            ),
        )
    }

    fn make_byte_parse_result_method_sig() -> MethodSig {
        Self::make_instance_method_sig(
            vec![],
            Type::Result(
                Box::new(Type::Primitive(PrimitiveType::Byte)),
                Box::new(Type::Named("ByteError".to_string(), Vec::new())),
            ),
        )
    }

    fn get_int_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "to_string" => Some(Self::make_to_string_method_sig()),
            "to_float" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Float),
            )),
            "to_int" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            "to_char" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Char),
            )),
            "to_byte" => Some(Self::make_byte_parse_result_method_sig()),
            "eq" => Some(Self::make_eq_method_sig(PrimitiveType::Int)),
            "cmp" => Some(Self::make_cmp_method_sig(PrimitiveType::Int)),
            "hash" => Some(Self::make_hash_method_sig()),
            _ => None,
        }
    }

    fn get_byte_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "to_string" => Some(Self::make_to_string_method_sig()),
            "to_int" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            "to_byte" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Byte),
            )),
            "eq" => Some(Self::make_eq_method_sig(PrimitiveType::Byte)),
            "cmp" => Some(Self::make_cmp_method_sig(PrimitiveType::Byte)),
            "hash" => Some(Self::make_hash_method_sig()),
            "checked_add" | "checked_sub" | "checked_mul" | "checked_div" | "checked_rem" => {
                Some(Self::make_instance_method_sig(
                    vec![Type::Primitive(PrimitiveType::Byte)],
                    Type::Result(
                        Box::new(Type::Primitive(PrimitiveType::Byte)),
                        Box::new(Type::Named("ByteError".to_string(), Vec::new())),
                    ),
                ))
            }
            "wrapping_add" | "wrapping_sub" | "wrapping_mul" | "saturating_add"
            | "saturating_sub" | "saturating_mul" | "bit_and" | "bit_or" | "bit_xor"
            | "rotate_left" | "rotate_right" => Some(Self::make_instance_method_sig(
                vec![if matches!(method_name, "rotate_left" | "rotate_right") {
                    Type::Primitive(PrimitiveType::Int)
                } else {
                    Type::Primitive(PrimitiveType::Byte)
                }],
                Type::Primitive(PrimitiveType::Byte),
            )),
            "bit_not" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Byte),
            )),
            "shift_left" | "shift_right" => Some(Self::make_instance_method_sig(
                vec![Type::Primitive(PrimitiveType::Int)],
                Type::Result(
                    Box::new(Type::Primitive(PrimitiveType::Byte)),
                    Box::new(Type::Named("ByteError".to_string(), Vec::new())),
                ),
            )),
            _ => None,
        }
    }

    fn get_bytes_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        let int = Type::Primitive(PrimitiveType::Int);
        let byte = Type::Primitive(PrimitiveType::Byte);
        let bytes = Type::Primitive(PrimitiveType::Bytes);
        let bytes_error = Type::Named("BytesError".to_string(), Vec::new());
        match method_name {
            "to_string" => Some(Self::make_to_string_method_sig()),
            "to_list" => Some(Self::make_instance_method_sig(
                vec![],
                Type::List(Box::new(byte.clone())),
            )),
            "to_utf8" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Result(
                    Box::new(Type::Primitive(PrimitiveType::Str)),
                    Box::new(bytes_error.clone()),
                ),
            )),
            "to_utf8_lossy" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Str),
            )),
            "cursor" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Result(
                    Box::new(Type::Named("BytesCursor".to_string(), Vec::new())),
                    Box::new(bytes_error.clone()),
                ),
            )),
            "len" | "size" => Some(Self::make_instance_method_sig(vec![], int)),
            "is_empty" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Bool),
            )),
            "get" => Some(Self::make_instance_method_sig(
                vec![int],
                Type::Optional(Box::new(byte)),
            )),
            "push_back" | "push_front" => Some(Self::make_instance_method_sig(
                vec![byte.clone()],
                Type::Primitive(PrimitiveType::Void),
            )),
            "pop_back" | "pop_front" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Optional(Box::new(byte)),
            )),
            "clear" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Void),
            )),
            "reserve" | "truncate" => Some(Self::make_instance_method_sig(
                vec![int],
                Type::Primitive(PrimitiveType::Void),
            )),
            "resize" => Some(Self::make_instance_method_sig(
                vec![int, byte.clone()],
                Type::Primitive(PrimitiveType::Void),
            )),
            "fill" => Some(Self::make_instance_method_sig(
                vec![byte.clone()],
                Type::Primitive(PrimitiveType::Void),
            )),
            "extend" => Some(Self::make_instance_method_sig(
                vec![bytes],
                Type::Primitive(PrimitiveType::Void),
            )),
            "contains" => Some(Self::make_instance_method_sig(
                vec![byte.clone()],
                Type::Primitive(PrimitiveType::Bool),
            )),
            "find" => Some(Self::make_instance_method_sig(
                vec![byte],
                Type::Optional(Box::new(int)),
            )),
            "insert" => Some(Self::make_instance_method_sig(
                vec![int, Type::Primitive(PrimitiveType::Byte)],
                Type::Primitive(PrimitiveType::Void),
            )),
            "remove" => Some(Self::make_instance_method_sig(
                vec![int],
                Type::Optional(Box::new(Type::Primitive(PrimitiveType::Byte))),
            )),
            "copy_within" => Some(Self::make_instance_method_sig(
                vec![int.clone(), int.clone(), int.clone()],
                Type::Primitive(PrimitiveType::Void),
            )),
            "format" => Some(Self::make_instance_method_sig(
                vec![int.clone(), int.clone()],
                Type::Result(
                    Box::new(Type::Primitive(PrimitiveType::Str)),
                    Box::new(bytes_error.clone()),
                ),
            )),
            "to_binary" | "to_octal" | "to_decimal" | "to_hex" => {
                Some(Self::make_instance_method_sig(
                    vec![],
                    Type::Result(
                        Box::new(Type::Primitive(PrimitiveType::Str)),
                        Box::new(bytes_error.clone()),
                    ),
                ))
            }
            "read_uint_le" | "read_uint_be" => Some(Self::make_instance_method_sig(
                vec![int.clone(), int.clone()],
                Type::Result(Box::new(int.clone()), Box::new(bytes_error.clone())),
            )),
            "write_uint_le" | "write_uint_be" => Some(Self::make_instance_method_sig(
                vec![int.clone(), int.clone(), int.clone()],
                Type::Result(
                    Box::new(Type::Primitive(PrimitiveType::Void)),
                    Box::new(bytes_error.clone()),
                ),
            )),
            "read_float_le" | "read_float_be" => Some(Self::make_instance_method_sig(
                vec![int.clone()],
                Type::Result(
                    Box::new(Type::Primitive(PrimitiveType::Float)),
                    Box::new(bytes_error.clone()),
                ),
            )),
            "write_float_le" | "write_float_be" => Some(Self::make_instance_method_sig(
                vec![int, Type::Primitive(PrimitiveType::Float)],
                Type::Result(
                    Box::new(Type::Primitive(PrimitiveType::Void)),
                    Box::new(bytes_error.clone()),
                ),
            )),
            "read_varint" => Some(Self::make_instance_method_sig(
                vec![int.clone()],
                Type::Result(Box::new(int.clone()), Box::new(bytes_error)),
            )),
            "write_varint" => Some(Self::make_instance_method_sig(
                vec![int.clone(), int.clone()],
                Type::Result(Box::new(int), Box::new(bytes_error)),
            )),
            _ => None,
        }
    }

    fn get_float_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "to_string" => Some(Self::make_to_string_method_sig()),
            "to_int" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            "to_float" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Float),
            )),
            "eq" => Some(Self::make_eq_method_sig(PrimitiveType::Float)),
            "cmp" => Some(Self::make_cmp_method_sig(PrimitiveType::Float)),
            "hash" => Some(Self::make_hash_method_sig()),
            _ => None,
        }
    }

    fn get_string_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "to_string" | "message" => Some(Self::make_to_string_method_sig()),
            "length" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            "to_int" => Some(Self::make_str_parse_result_method_sig(PrimitiveType::Int)),
            "to_float" => Some(Self::make_str_parse_result_method_sig(PrimitiveType::Float)),
            "to_char" => Some(Self::make_str_parse_result_method_sig(PrimitiveType::Char)),
            "to_byte" => Some(Self::make_byte_parse_result_method_sig()),
            "eq" => Some(Self::make_eq_method_sig(PrimitiveType::Str)),
            "cmp" => Some(Self::make_cmp_method_sig(PrimitiveType::Str)),
            "hash" => Some(Self::make_hash_method_sig()),
            // Decomposition. Positions are characters, matching `length`.
            "split" => Some(Self::make_instance_method_sig(
                vec![Type::Primitive(PrimitiveType::Str)],
                Type::List(Box::new(Type::Primitive(PrimitiveType::Str))),
            )),
            "char_at" => Some(Self::make_instance_method_sig(
                vec![Type::Primitive(PrimitiveType::Int)],
                Type::Optional(Box::new(Type::Primitive(PrimitiveType::Char))),
            )),
            "substring" => Some(Self::make_instance_method_sig(
                vec![
                    Type::Primitive(PrimitiveType::Int),
                    Type::Primitive(PrimitiveType::Int),
                ],
                Type::Primitive(PrimitiveType::Str),
            )),
            // The characters as a list, which is also what makes
            // `for char c in s` work through the existing list loop.
            "to_list" => Some(Self::make_instance_method_sig(
                vec![],
                Type::List(Box::new(Type::Primitive(PrimitiveType::Char))),
            )),
            "trim" | "to_upper" | "to_lower" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Str),
            )),
            "starts_with" | "ends_with" | "contains" => Some(Self::make_instance_method_sig(
                vec![Type::Primitive(PrimitiveType::Str)],
                Type::Primitive(PrimitiveType::Bool),
            )),
            // -1 when absent, so this is an int rather than an optional: a
            // caller almost always compares it, and `>= 0` reads better than
            // opening an optional to ask the same question.
            "index_of" => Some(Self::make_instance_method_sig(
                vec![Type::Primitive(PrimitiveType::Str)],
                Type::Primitive(PrimitiveType::Int),
            )),
            "replace" => Some(Self::make_instance_method_sig(
                vec![
                    Type::Primitive(PrimitiveType::Str),
                    Type::Primitive(PrimitiveType::Str),
                ],
                Type::Primitive(PrimitiveType::Str),
            )),
            _ => None,
        }
    }

    fn get_bool_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "to_string" => Some(Self::make_to_string_method_sig()),
            "to_int" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            "to_float" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Float),
            )),
            "eq" => Some(Self::make_eq_method_sig(PrimitiveType::Bool)),
            "hash" => Some(Self::make_hash_method_sig()),
            _ => None,
        }
    }

    fn get_char_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "to_string" => Some(Self::make_to_string_method_sig()),
            "to_int" => Some(Self::make_str_parse_result_method_sig(PrimitiveType::Int)),
            "to_codepoint" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            "to_char" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Char),
            )),
            "eq" => Some(Self::make_eq_method_sig(PrimitiveType::Char)),
            "cmp" => Some(Self::make_cmp_method_sig(PrimitiveType::Char)),
            "hash" => Some(Self::make_hash_method_sig()),
            _ => None,
        }
    }

    fn get_list_method_sig(&self, elem_type: &Type, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "push_back" | "push" => Some(MethodSig {
                params: vec![elem_type.clone()],
                return_type: Type::Void,
                is_static: false,
            }),
            "pop_back" | "pop" => Some(MethodSig {
                params: vec![],
                return_type: Type::Optional(Box::new(elem_type.clone())),
                is_static: false,
            }),
            "get" => Some(MethodSig {
                params: vec![Type::Primitive(PrimitiveType::Int)],
                return_type: Type::Optional(Box::new(elem_type.clone())),
                is_static: false,
            }),
            "is_empty" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            // `len` is the `Collection<T>` spelling of `size`; both are kept so
            // existing code and the interface agree.
            "size" | "len" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Int),
                is_static: false,
            }),
            "contains" => Some(MethodSig {
                params: vec![elem_type.clone()],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            // Identity for a list, but required by `Collection<T>` so a list can
            // be passed to the generic algorithms.
            "to_list" => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(elem_type.clone())),
                is_static: false,
            }),
            "to_bytes" if matches!(elem_type, Type::Primitive(PrimitiveType::Byte)) => {
                Some(MethodSig {
                    params: vec![],
                    return_type: Type::Result(
                        Box::new(Type::Primitive(PrimitiveType::Bytes)),
                        Box::new(Type::Named("BytesError".to_string(), Vec::new())),
                    ),
                    is_static: false,
                })
            }
            "to_string" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            _ => None,
        }
    }

    fn get_map_method_sig(
        &self,
        key_type: &Type,
        value_type: &Type,
        method_name: &str,
    ) -> Option<MethodSig> {
        match method_name {
            "put" => Some(MethodSig {
                params: vec![key_type.clone(), value_type.clone()],
                return_type: Type::Void,
                is_static: false,
            }),
            "get" | "remove" => Some(MethodSig {
                params: vec![key_type.clone()],
                return_type: Type::Optional(Box::new(value_type.clone())),
                is_static: false,
            }),
            "get_keys" => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(key_type.clone())),
                is_static: false,
            }),
            "get_values" => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(value_type.clone())),
                is_static: false,
            }),
            "get_pairs" | "to_list" => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(Type::Tuple(
                    Box::new(key_type.clone()),
                    Box::new(value_type.clone()),
                ))),
                is_static: false,
            }),
            "contains" => Some(MethodSig {
                params: vec![key_type.clone()],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            "size" | "len" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Int),
                is_static: false,
            }),
            "is_empty" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            "to_string" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            _ => None,
        }
    }

    fn get_set_method_sig(&self, elem_type: &Type, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "add" => Some(MethodSig {
                params: vec![elem_type.clone()],
                return_type: Type::Void,
                is_static: false,
            }),
            "remove" | "contains" => Some(MethodSig {
                params: vec![elem_type.clone()],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            "size" | "len" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Int),
                is_static: false,
            }),
            "is_empty" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            "to_string" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            "to_list" => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(elem_type.clone())),
                is_static: false,
            }),
            _ => None,
        }
    }

    fn get_optional_method_sig(&self, inner: &Type, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "is_some" | "is_none" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            "value" => Some(MethodSig {
                params: vec![],
                return_type: inner.clone(),
                is_static: false,
            }),
            "to_string" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            _ => None,
        }
    }

    fn get_result_method_sig(
        &self,
        ok: &Type,
        error: &Type,
        method_name: &str,
    ) -> Option<MethodSig> {
        match method_name {
            "is_ok" | "is_err" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            "value" | "error" => Some(MethodSig {
                params: vec![],
                return_type: if method_name == "value" {
                    ok.clone()
                } else {
                    error.clone()
                },
                is_static: false,
            }),
            "to_string" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            _ => None,
        }
    }

    fn get_tuple_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "to_string" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            "new" => Some(MethodSig {
                params: vec![],
                return_type: Type::Tuple(
                    Box::new(Type::Primitive(PrimitiveType::Int)),
                    Box::new(Type::Primitive(PrimitiveType::Str)),
                ),
                is_static: true,
            }),
            _ => None,
        }
    }

    pub(crate) fn get_method_sig(&self, type_: &Type, method_name: &str) -> Option<MethodSig> {
        match type_ {
            Type::Named(name, args) => self.get_named_method_sig(name, args, method_name),
            Type::Variable(var) | Type::Generic(var) => {
                self.get_variable_generic_method_sig(var, method_name)
            }
            Type::Primitive(prim) => self.get_primitive_method_sig(prim, method_name),
            Type::List(elem_type) => self.get_list_method_sig(elem_type, method_name),
            Type::Map(key_type, value_type) => {
                self.get_map_method_sig(key_type, value_type, method_name)
            }
            Type::Set(elem_type) => self.get_set_method_sig(elem_type, method_name),
            Type::Optional(inner) => self.get_optional_method_sig(inner, method_name),
            Type::Result(ok, error) => self.get_result_method_sig(ok, error, method_name),
            Type::Tuple(_, _) => self.get_tuple_method_sig(method_name),
            Type::Reference(inner) => self.get_method_sig(inner, method_name),
            Type::TraitObject(inner) => self.get_method_sig(inner, method_name),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MethodSig, PrimitiveType, SemanticAnalyzer, Type};

    fn primitive(primitive: PrimitiveType) -> Type {
        Type::Primitive(primitive)
    }

    #[test]
    fn primitive_method_signatures_keep_parameter_and_return_types() {
        let analyzer = SemanticAnalyzer::new();

        assert_eq!(
            analyzer.get_method_sig(&primitive(PrimitiveType::Int), "cmp"),
            Some(MethodSig {
                params: vec![primitive(PrimitiveType::Int)],
                return_type: primitive(PrimitiveType::Int),
                is_static: false,
            })
        );
        assert_eq!(
            analyzer.get_method_sig(&primitive(PrimitiveType::Str), "char_at"),
            Some(MethodSig {
                params: vec![primitive(PrimitiveType::Int)],
                return_type: Type::Optional(Box::new(primitive(PrimitiveType::Char))),
                is_static: false,
            })
        );
    }

    #[test]
    fn collection_method_signatures_preserve_receiver_types() {
        let analyzer = SemanticAnalyzer::new();
        let string = primitive(PrimitiveType::Str);
        let boolean = primitive(PrimitiveType::Bool);

        assert_eq!(
            analyzer.get_method_sig(&Type::List(Box::new(string.clone())), "get"),
            Some(MethodSig {
                params: vec![primitive(PrimitiveType::Int)],
                return_type: Type::Optional(Box::new(string.clone())),
                is_static: false,
            })
        );
        assert_eq!(
            analyzer.get_method_sig(
                &Type::Map(Box::new(string.clone()), Box::new(boolean.clone())),
                "put",
            ),
            Some(MethodSig {
                params: vec![string, boolean],
                return_type: Type::Void,
                is_static: false,
            })
        );
    }

    #[test]
    fn sum_type_inspection_methods_have_typed_signatures() {
        let analyzer = SemanticAnalyzer::new();
        let optional = Type::Optional(Box::new(primitive(PrimitiveType::Int)));
        let result = Type::Result(
            Box::new(primitive(PrimitiveType::Int)),
            Box::new(primitive(PrimitiveType::Str)),
        );

        for method in ["is_some", "is_none"] {
            assert_eq!(
                analyzer
                    .get_method_sig(&optional, method)
                    .unwrap()
                    .return_type,
                primitive(PrimitiveType::Bool)
            );
        }
        for method in ["is_ok", "is_err"] {
            assert_eq!(
                analyzer
                    .get_method_sig(&result, method)
                    .unwrap()
                    .return_type,
                primitive(PrimitiveType::Bool)
            );
        }
        assert_eq!(
            analyzer
                .get_method_sig(&result, "value")
                .unwrap()
                .return_type,
            primitive(PrimitiveType::Int)
        );
        assert_eq!(
            analyzer
                .get_method_sig(&result, "error")
                .unwrap()
                .return_type,
            primitive(PrimitiveType::Str)
        );
        assert_eq!(
            analyzer
                .get_method_sig(&optional, "value")
                .unwrap()
                .return_type,
            primitive(PrimitiveType::Int)
        );
        assert!(analyzer.get_method_sig(&optional, "error").is_none());
    }
}
