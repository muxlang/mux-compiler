use super::{MethodSig, SemanticAnalyzer, Type};
use crate::ast::PrimitiveType;

macro_rules! define_builtin_methods {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        enum BuiltinMethod {
            $($variant),+
        }

        impl BuiltinMethod {
            const ALL: &'static [Self] = &[$(Self::$variant),+];

            fn parse(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$variant),)+
                    _ => None,
                }
            }

            fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }
        }
    };
}

define_builtin_methods! {
    Add => "add",
    BitAnd => "bit_and",
    BitNot => "bit_not",
    BitOr => "bit_or",
    BitXor => "bit_xor",
    CharAt => "char_at",
    CheckedAdd => "checked_add",
    CheckedDiv => "checked_div",
    CheckedMul => "checked_mul",
    CheckedRem => "checked_rem",
    CheckedSub => "checked_sub",
    Clear => "clear",
    Cmp => "cmp",
    Contains => "contains",
    CopyWithin => "copy_within",
    Cursor => "cursor",
    EndsWith => "ends_with",
    Eq => "eq",
    Error => "error",
    Extend => "extend",
    Fill => "fill",
    Find => "find",
    Format => "format",
    Get => "get",
    GetKeys => "get_keys",
    GetPairs => "get_pairs",
    GetValues => "get_values",
    Hash => "hash",
    IndexOf => "index_of",
    Insert => "insert",
    IsEmpty => "is_empty",
    IsErr => "is_err",
    IsNone => "is_none",
    IsOk => "is_ok",
    IsSome => "is_some",
    Len => "len",
    Length => "length",
    Message => "message",
    New => "new",
    Pop => "pop",
    PopBack => "pop_back",
    PopFront => "pop_front",
    Push => "push",
    PushBack => "push_back",
    PushFront => "push_front",
    Put => "put",
    ReadFloatBe => "read_float_be",
    ReadFloatLe => "read_float_le",
    ReadUintBe => "read_uint_be",
    ReadUintLe => "read_uint_le",
    ReadVarint => "read_varint",
    Remove => "remove",
    Replace => "replace",
    Reserve => "reserve",
    Resize => "resize",
    RotateLeft => "rotate_left",
    RotateRight => "rotate_right",
    SaturatingAdd => "saturating_add",
    SaturatingMul => "saturating_mul",
    SaturatingSub => "saturating_sub",
    ShiftLeft => "shift_left",
    ShiftRight => "shift_right",
    Size => "size",
    Split => "split",
    StartsWith => "starts_with",
    Substring => "substring",
    ToBinary => "to_binary",
    ToByte => "to_byte",
    ToBytes => "to_bytes",
    ToChar => "to_char",
    ToCodepoint => "to_codepoint",
    ToDecimal => "to_decimal",
    ToFloat => "to_float",
    ToHex => "to_hex",
    ToInt => "to_int",
    ToList => "to_list",
    ToLower => "to_lower",
    ToOctal => "to_octal",
    ToString => "to_string",
    ToUpper => "to_upper",
    ToUtf8 => "to_utf8",
    ToUtf8Lossy => "to_utf8_lossy",
    Trim => "trim",
    Truncate => "truncate",
    Value => "value",
    WrappingAdd => "wrapping_add",
    WrappingMul => "wrapping_mul",
    WrappingSub => "wrapping_sub",
    WriteFloatBe => "write_float_be",
    WriteFloatLe => "write_float_le",
    WriteUintBe => "write_uint_be",
    WriteUintLe => "write_uint_le",
    WriteVarint => "write_varint"
}

impl SemanticAnalyzer {
    /// Return built-in instance method names accepted by `type_`.
    ///
    /// The shared inventory drives signature lookup and editor completions.
    #[must_use]
    pub fn builtin_method_names(&self, type_: &Type) -> Vec<&'static str> {
        let candidates = BuiltinMethod::ALL;

        candidates
            .iter()
            .copied()
            .filter(|method| {
                self.get_method_sig(type_, method.name())
                    .is_some_and(|signature| !signature.is_static)
            })
            .map(BuiltinMethod::name)
            .collect()
    }

    fn get_primitive_method_sig(
        &self,
        prim: &PrimitiveType,
        method_name: BuiltinMethod,
    ) -> Option<MethodSig> {
        use PrimitiveType::{Bool, Byte, Bytes, Char, Float, Int, Str};
        let resolver = match prim {
            Int => Some(Self::get_int_method_sig as fn(&Self, BuiltinMethod) -> Option<MethodSig>),
            Byte => {
                Some(Self::get_byte_method_sig as fn(&Self, BuiltinMethod) -> Option<MethodSig>)
            }
            Float => {
                Some(Self::get_float_method_sig as fn(&Self, BuiltinMethod) -> Option<MethodSig>)
            }
            Str => {
                Some(Self::get_string_method_sig as fn(&Self, BuiltinMethod) -> Option<MethodSig>)
            }
            Bytes => {
                Some(Self::get_bytes_method_sig as fn(&Self, BuiltinMethod) -> Option<MethodSig>)
            }
            Bool => {
                Some(Self::get_bool_method_sig as fn(&Self, BuiltinMethod) -> Option<MethodSig>)
            }
            Char => {
                Some(Self::get_char_method_sig as fn(&Self, BuiltinMethod) -> Option<MethodSig>)
            }
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

    fn get_int_method_sig(&self, method_name: BuiltinMethod) -> Option<MethodSig> {
        match method_name {
            BuiltinMethod::ToString => Some(Self::make_to_string_method_sig()),
            BuiltinMethod::ToFloat => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Float),
            )),
            BuiltinMethod::ToInt => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            BuiltinMethod::ToChar => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Char),
            )),
            BuiltinMethod::ToByte => Some(Self::make_byte_parse_result_method_sig()),
            BuiltinMethod::Eq => Some(Self::make_eq_method_sig(PrimitiveType::Int)),
            BuiltinMethod::Cmp => Some(Self::make_cmp_method_sig(PrimitiveType::Int)),
            BuiltinMethod::Hash => Some(Self::make_hash_method_sig()),
            _ => None,
        }
    }

    fn get_byte_method_sig(&self, method_name: BuiltinMethod) -> Option<MethodSig> {
        match method_name {
            BuiltinMethod::ToString => Some(Self::make_to_string_method_sig()),
            BuiltinMethod::ToInt => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            BuiltinMethod::ToByte => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Byte),
            )),
            BuiltinMethod::Eq => Some(Self::make_eq_method_sig(PrimitiveType::Byte)),
            BuiltinMethod::Cmp => Some(Self::make_cmp_method_sig(PrimitiveType::Byte)),
            BuiltinMethod::Hash => Some(Self::make_hash_method_sig()),
            BuiltinMethod::CheckedAdd
            | BuiltinMethod::CheckedSub
            | BuiltinMethod::CheckedMul
            | BuiltinMethod::CheckedDiv
            | BuiltinMethod::CheckedRem => Some(Self::make_instance_method_sig(
                vec![Type::Primitive(PrimitiveType::Byte)],
                Type::Result(
                    Box::new(Type::Primitive(PrimitiveType::Byte)),
                    Box::new(Type::Named("ByteError".to_string(), Vec::new())),
                ),
            )),
            BuiltinMethod::WrappingAdd
            | BuiltinMethod::WrappingSub
            | BuiltinMethod::WrappingMul
            | BuiltinMethod::SaturatingAdd
            | BuiltinMethod::SaturatingSub
            | BuiltinMethod::SaturatingMul
            | BuiltinMethod::BitAnd
            | BuiltinMethod::BitOr
            | BuiltinMethod::BitXor
            | BuiltinMethod::RotateLeft
            | BuiltinMethod::RotateRight => Some(Self::make_instance_method_sig(
                vec![if matches!(
                    method_name,
                    BuiltinMethod::RotateLeft | BuiltinMethod::RotateRight
                ) {
                    Type::Primitive(PrimitiveType::Int)
                } else {
                    Type::Primitive(PrimitiveType::Byte)
                }],
                Type::Primitive(PrimitiveType::Byte),
            )),
            BuiltinMethod::BitNot => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Byte),
            )),
            BuiltinMethod::ShiftLeft | BuiltinMethod::ShiftRight => {
                Some(Self::make_instance_method_sig(
                    vec![Type::Primitive(PrimitiveType::Int)],
                    Type::Result(
                        Box::new(Type::Primitive(PrimitiveType::Byte)),
                        Box::new(Type::Named("ByteError".to_string(), Vec::new())),
                    ),
                ))
            }
            _ => None,
        }
    }

    fn get_bytes_method_sig(&self, method_name: BuiltinMethod) -> Option<MethodSig> {
        let int = Type::Primitive(PrimitiveType::Int);
        let byte = Type::Primitive(PrimitiveType::Byte);
        let bytes = Type::Primitive(PrimitiveType::Bytes);
        let bytes_error = Type::Named("BytesError".to_string(), Vec::new());
        match method_name {
            BuiltinMethod::ToString => Some(Self::make_to_string_method_sig()),
            BuiltinMethod::ToList => Some(Self::make_instance_method_sig(
                vec![],
                Type::List(Box::new(byte.clone())),
            )),
            BuiltinMethod::ToUtf8 => Some(Self::make_instance_method_sig(
                vec![],
                Type::Result(
                    Box::new(Type::Primitive(PrimitiveType::Str)),
                    Box::new(bytes_error.clone()),
                ),
            )),
            BuiltinMethod::ToUtf8Lossy => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Str),
            )),
            BuiltinMethod::Cursor => Some(Self::make_instance_method_sig(
                vec![],
                Type::Result(
                    Box::new(Type::Named("BytesCursor".to_string(), Vec::new())),
                    Box::new(bytes_error.clone()),
                ),
            )),
            BuiltinMethod::Len | BuiltinMethod::Size => {
                Some(Self::make_instance_method_sig(vec![], int))
            }
            BuiltinMethod::IsEmpty => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Bool),
            )),
            BuiltinMethod::Get => Some(Self::make_instance_method_sig(
                vec![int],
                Type::Optional(Box::new(byte)),
            )),
            BuiltinMethod::PushBack | BuiltinMethod::PushFront => {
                Some(Self::make_instance_method_sig(
                    vec![byte.clone()],
                    Type::Primitive(PrimitiveType::Void),
                ))
            }
            BuiltinMethod::PopBack | BuiltinMethod::PopFront => Some(
                Self::make_instance_method_sig(vec![], Type::Optional(Box::new(byte))),
            ),
            BuiltinMethod::Clear => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Void),
            )),
            BuiltinMethod::Reserve | BuiltinMethod::Truncate => Some(
                Self::make_instance_method_sig(vec![int], Type::Primitive(PrimitiveType::Void)),
            ),
            BuiltinMethod::Resize => Some(Self::make_instance_method_sig(
                vec![int, byte.clone()],
                Type::Primitive(PrimitiveType::Void),
            )),
            BuiltinMethod::Fill => Some(Self::make_instance_method_sig(
                vec![byte.clone()],
                Type::Primitive(PrimitiveType::Void),
            )),
            BuiltinMethod::Extend => Some(Self::make_instance_method_sig(
                vec![bytes],
                Type::Primitive(PrimitiveType::Void),
            )),
            BuiltinMethod::Contains => Some(Self::make_instance_method_sig(
                vec![byte.clone()],
                Type::Primitive(PrimitiveType::Bool),
            )),
            BuiltinMethod::Find => Some(Self::make_instance_method_sig(
                vec![byte],
                Type::Optional(Box::new(int)),
            )),
            BuiltinMethod::Insert => Some(Self::make_instance_method_sig(
                vec![int, Type::Primitive(PrimitiveType::Byte)],
                Type::Primitive(PrimitiveType::Void),
            )),
            BuiltinMethod::Remove => Some(Self::make_instance_method_sig(
                vec![int],
                Type::Optional(Box::new(Type::Primitive(PrimitiveType::Byte))),
            )),
            BuiltinMethod::CopyWithin => Some(Self::make_instance_method_sig(
                vec![int.clone(), int.clone(), int.clone()],
                Type::Primitive(PrimitiveType::Void),
            )),
            BuiltinMethod::Format => Some(Self::make_instance_method_sig(
                vec![int.clone(), int.clone()],
                Type::Result(
                    Box::new(Type::Primitive(PrimitiveType::Str)),
                    Box::new(bytes_error.clone()),
                ),
            )),
            BuiltinMethod::ToBinary
            | BuiltinMethod::ToOctal
            | BuiltinMethod::ToDecimal
            | BuiltinMethod::ToHex => Some(Self::make_instance_method_sig(
                vec![],
                Type::Result(
                    Box::new(Type::Primitive(PrimitiveType::Str)),
                    Box::new(bytes_error.clone()),
                ),
            )),
            BuiltinMethod::ReadUintLe | BuiltinMethod::ReadUintBe => {
                Some(Self::make_instance_method_sig(
                    vec![int.clone(), int.clone()],
                    Type::Result(Box::new(int.clone()), Box::new(bytes_error.clone())),
                ))
            }
            BuiltinMethod::WriteUintLe | BuiltinMethod::WriteUintBe => {
                Some(Self::make_instance_method_sig(
                    vec![int.clone(), int.clone(), int.clone()],
                    Type::Result(
                        Box::new(Type::Primitive(PrimitiveType::Void)),
                        Box::new(bytes_error.clone()),
                    ),
                ))
            }
            BuiltinMethod::ReadFloatLe | BuiltinMethod::ReadFloatBe => {
                Some(Self::make_instance_method_sig(
                    vec![int.clone()],
                    Type::Result(
                        Box::new(Type::Primitive(PrimitiveType::Float)),
                        Box::new(bytes_error.clone()),
                    ),
                ))
            }
            BuiltinMethod::WriteFloatLe | BuiltinMethod::WriteFloatBe => {
                Some(Self::make_instance_method_sig(
                    vec![int, Type::Primitive(PrimitiveType::Float)],
                    Type::Result(
                        Box::new(Type::Primitive(PrimitiveType::Void)),
                        Box::new(bytes_error.clone()),
                    ),
                ))
            }
            BuiltinMethod::ReadVarint => Some(Self::make_instance_method_sig(
                vec![int.clone()],
                Type::Result(Box::new(int.clone()), Box::new(bytes_error)),
            )),
            BuiltinMethod::WriteVarint => Some(Self::make_instance_method_sig(
                vec![int.clone(), int.clone()],
                Type::Result(Box::new(int), Box::new(bytes_error)),
            )),
            _ => None,
        }
    }

    fn get_float_method_sig(&self, method_name: BuiltinMethod) -> Option<MethodSig> {
        match method_name {
            BuiltinMethod::ToString => Some(Self::make_to_string_method_sig()),
            BuiltinMethod::ToInt => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            BuiltinMethod::ToFloat => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Float),
            )),
            BuiltinMethod::Eq => Some(Self::make_eq_method_sig(PrimitiveType::Float)),
            BuiltinMethod::Cmp => Some(Self::make_cmp_method_sig(PrimitiveType::Float)),
            BuiltinMethod::Hash => Some(Self::make_hash_method_sig()),
            _ => None,
        }
    }

    fn get_string_method_sig(&self, method_name: BuiltinMethod) -> Option<MethodSig> {
        match method_name {
            BuiltinMethod::ToString | BuiltinMethod::Message => {
                Some(Self::make_to_string_method_sig())
            }
            BuiltinMethod::Length => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            BuiltinMethod::ToInt => {
                Some(Self::make_str_parse_result_method_sig(PrimitiveType::Int))
            }
            BuiltinMethod::ToFloat => {
                Some(Self::make_str_parse_result_method_sig(PrimitiveType::Float))
            }
            BuiltinMethod::ToChar => {
                Some(Self::make_str_parse_result_method_sig(PrimitiveType::Char))
            }
            BuiltinMethod::ToByte => Some(Self::make_byte_parse_result_method_sig()),
            BuiltinMethod::Eq => Some(Self::make_eq_method_sig(PrimitiveType::Str)),
            BuiltinMethod::Cmp => Some(Self::make_cmp_method_sig(PrimitiveType::Str)),
            BuiltinMethod::Hash => Some(Self::make_hash_method_sig()),
            // Decomposition. Positions are characters, matching `length`.
            BuiltinMethod::Split => Some(Self::make_instance_method_sig(
                vec![Type::Primitive(PrimitiveType::Str)],
                Type::List(Box::new(Type::Primitive(PrimitiveType::Str))),
            )),
            BuiltinMethod::CharAt => Some(Self::make_instance_method_sig(
                vec![Type::Primitive(PrimitiveType::Int)],
                Type::Optional(Box::new(Type::Primitive(PrimitiveType::Char))),
            )),
            BuiltinMethod::Substring => Some(Self::make_instance_method_sig(
                vec![
                    Type::Primitive(PrimitiveType::Int),
                    Type::Primitive(PrimitiveType::Int),
                ],
                Type::Primitive(PrimitiveType::Str),
            )),
            // The characters as a list, which is also what makes
            // `for char c in s` work through the existing list loop.
            BuiltinMethod::ToList => Some(Self::make_instance_method_sig(
                vec![],
                Type::List(Box::new(Type::Primitive(PrimitiveType::Char))),
            )),
            BuiltinMethod::Trim | BuiltinMethod::ToUpper | BuiltinMethod::ToLower => Some(
                Self::make_instance_method_sig(vec![], Type::Primitive(PrimitiveType::Str)),
            ),
            BuiltinMethod::StartsWith | BuiltinMethod::EndsWith | BuiltinMethod::Contains => {
                Some(Self::make_instance_method_sig(
                    vec![Type::Primitive(PrimitiveType::Str)],
                    Type::Primitive(PrimitiveType::Bool),
                ))
            }
            // -1 when absent, so this is an int rather than an optional: a
            // caller almost always compares it, and `>= 0` reads better than
            // opening an optional to ask the same question.
            BuiltinMethod::IndexOf => Some(Self::make_instance_method_sig(
                vec![Type::Primitive(PrimitiveType::Str)],
                Type::Primitive(PrimitiveType::Int),
            )),
            BuiltinMethod::Replace => Some(Self::make_instance_method_sig(
                vec![
                    Type::Primitive(PrimitiveType::Str),
                    Type::Primitive(PrimitiveType::Str),
                ],
                Type::Primitive(PrimitiveType::Str),
            )),
            _ => None,
        }
    }

    fn get_bool_method_sig(&self, method_name: BuiltinMethod) -> Option<MethodSig> {
        match method_name {
            BuiltinMethod::ToString => Some(Self::make_to_string_method_sig()),
            BuiltinMethod::ToInt => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            BuiltinMethod::ToFloat => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Float),
            )),
            BuiltinMethod::Eq => Some(Self::make_eq_method_sig(PrimitiveType::Bool)),
            BuiltinMethod::Hash => Some(Self::make_hash_method_sig()),
            _ => None,
        }
    }

    fn get_char_method_sig(&self, method_name: BuiltinMethod) -> Option<MethodSig> {
        match method_name {
            BuiltinMethod::ToString => Some(Self::make_to_string_method_sig()),
            BuiltinMethod::ToInt => {
                Some(Self::make_str_parse_result_method_sig(PrimitiveType::Int))
            }
            BuiltinMethod::ToCodepoint => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            BuiltinMethod::ToChar => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Char),
            )),
            BuiltinMethod::Eq => Some(Self::make_eq_method_sig(PrimitiveType::Char)),
            BuiltinMethod::Cmp => Some(Self::make_cmp_method_sig(PrimitiveType::Char)),
            BuiltinMethod::Hash => Some(Self::make_hash_method_sig()),
            _ => None,
        }
    }

    fn get_list_method_sig(
        &self,
        elem_type: &Type,
        method_name: BuiltinMethod,
    ) -> Option<MethodSig> {
        match method_name {
            BuiltinMethod::PushBack | BuiltinMethod::Push => Some(MethodSig {
                params: vec![elem_type.clone()],
                return_type: Type::Void,
                is_static: false,
            }),
            BuiltinMethod::PopBack | BuiltinMethod::Pop => Some(MethodSig {
                params: vec![],
                return_type: Type::Optional(Box::new(elem_type.clone())),
                is_static: false,
            }),
            BuiltinMethod::Get => Some(MethodSig {
                params: vec![Type::Primitive(PrimitiveType::Int)],
                return_type: Type::Optional(Box::new(elem_type.clone())),
                is_static: false,
            }),
            BuiltinMethod::IsEmpty => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            // `len` is the `Collection<T>` spelling of `size`; both are kept so
            // existing code and the interface agree.
            BuiltinMethod::Size | BuiltinMethod::Len => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Int),
                is_static: false,
            }),
            BuiltinMethod::Contains => Some(MethodSig {
                params: vec![elem_type.clone()],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            // Identity for a list, but required by `Collection<T>` so a list can
            // be passed to the generic algorithms.
            BuiltinMethod::ToList => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(elem_type.clone())),
                is_static: false,
            }),
            BuiltinMethod::ToBytes if matches!(elem_type, Type::Primitive(PrimitiveType::Byte)) => {
                Some(MethodSig {
                    params: vec![],
                    return_type: Type::Result(
                        Box::new(Type::Primitive(PrimitiveType::Bytes)),
                        Box::new(Type::Named("BytesError".to_string(), Vec::new())),
                    ),
                    is_static: false,
                })
            }
            BuiltinMethod::ToString => Some(MethodSig {
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
        method_name: BuiltinMethod,
    ) -> Option<MethodSig> {
        match method_name {
            BuiltinMethod::Put => Some(MethodSig {
                params: vec![key_type.clone(), value_type.clone()],
                return_type: Type::Void,
                is_static: false,
            }),
            BuiltinMethod::Get | BuiltinMethod::Remove => Some(MethodSig {
                params: vec![key_type.clone()],
                return_type: Type::Optional(Box::new(value_type.clone())),
                is_static: false,
            }),
            BuiltinMethod::GetKeys => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(key_type.clone())),
                is_static: false,
            }),
            BuiltinMethod::GetValues => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(value_type.clone())),
                is_static: false,
            }),
            BuiltinMethod::GetPairs | BuiltinMethod::ToList => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(Type::Tuple(
                    Box::new(key_type.clone()),
                    Box::new(value_type.clone()),
                ))),
                is_static: false,
            }),
            BuiltinMethod::Contains => Some(MethodSig {
                params: vec![key_type.clone()],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            BuiltinMethod::Size | BuiltinMethod::Len => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Int),
                is_static: false,
            }),
            BuiltinMethod::IsEmpty => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            BuiltinMethod::ToString => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            _ => None,
        }
    }

    fn get_set_method_sig(
        &self,
        elem_type: &Type,
        method_name: BuiltinMethod,
    ) -> Option<MethodSig> {
        match method_name {
            BuiltinMethod::Add => Some(MethodSig {
                params: vec![elem_type.clone()],
                return_type: Type::Void,
                is_static: false,
            }),
            BuiltinMethod::Remove | BuiltinMethod::Contains => Some(MethodSig {
                params: vec![elem_type.clone()],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            BuiltinMethod::Size | BuiltinMethod::Len => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Int),
                is_static: false,
            }),
            BuiltinMethod::IsEmpty => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            BuiltinMethod::ToString => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            BuiltinMethod::ToList => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(elem_type.clone())),
                is_static: false,
            }),
            _ => None,
        }
    }

    fn get_optional_method_sig(
        &self,
        inner: &Type,
        method_name: BuiltinMethod,
    ) -> Option<MethodSig> {
        match method_name {
            BuiltinMethod::IsSome | BuiltinMethod::IsNone => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            BuiltinMethod::Value => Some(MethodSig {
                params: vec![],
                return_type: inner.clone(),
                is_static: false,
            }),
            BuiltinMethod::ToString => Some(MethodSig {
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
        method_name: BuiltinMethod,
    ) -> Option<MethodSig> {
        match method_name {
            BuiltinMethod::IsOk | BuiltinMethod::IsErr => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            BuiltinMethod::Value | BuiltinMethod::Error => Some(MethodSig {
                params: vec![],
                return_type: if method_name == BuiltinMethod::Value {
                    ok.clone()
                } else {
                    error.clone()
                },
                is_static: false,
            }),
            BuiltinMethod::ToString => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            _ => None,
        }
    }

    fn get_tuple_method_sig(&self, method_name: BuiltinMethod) -> Option<MethodSig> {
        match method_name {
            BuiltinMethod::ToString => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            BuiltinMethod::New => Some(MethodSig {
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
        let method = BuiltinMethod::parse(method_name);
        self.resolve_method_sig(type_, method_name, method)
    }

    fn resolve_method_sig(
        &self,
        type_: &Type,
        method_name: &str,
        builtin_method: Option<BuiltinMethod>,
    ) -> Option<MethodSig> {
        match type_ {
            Type::Named(name, args) => self.get_named_method_sig(name, args, method_name),
            Type::Variable(var) | Type::Generic(var) => {
                self.get_variable_generic_method_sig(var, method_name)
            }
            Type::Primitive(prim) => {
                builtin_method.and_then(|method| self.get_primitive_method_sig(prim, method))
            }
            Type::List(elem_type) => {
                builtin_method.and_then(|method| self.get_list_method_sig(elem_type, method))
            }
            Type::Map(key_type, value_type) => builtin_method
                .and_then(|method| self.get_map_method_sig(key_type, value_type, method)),
            Type::Set(elem_type) => {
                builtin_method.and_then(|method| self.get_set_method_sig(elem_type, method))
            }
            Type::Optional(inner) => {
                builtin_method.and_then(|method| self.get_optional_method_sig(inner, method))
            }
            Type::Result(ok, error) => {
                builtin_method.and_then(|method| self.get_result_method_sig(ok, error, method))
            }
            Type::Tuple(_, _) => {
                builtin_method.and_then(|method| self.get_tuple_method_sig(method))
            }
            Type::Reference(inner) | Type::TraitObject(inner) => {
                self.resolve_method_sig(inner, method_name, builtin_method)
            }
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
    fn bytes_completions_include_utf8_conversions() {
        let analyzer = SemanticAnalyzer::new();
        let names = analyzer.builtin_method_names(&primitive(PrimitiveType::Bytes));

        assert!(names.contains(&"to_utf8"));
        assert!(names.contains(&"to_utf8_lossy"));
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
