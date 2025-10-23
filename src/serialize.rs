#![allow(unused)]

use std::{borrow::Cow, error::Error, fmt};

use facet_core::{FieldAttribute, FieldFlags, NumericType, PrimitiveType, Type, UserType};
use facet_reflect::{Peek, PeekStruct, ReflectError, ScalarType};
use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

/// Serialize a struct into a kdl document.
pub fn to_string<'a, T: facet_core::Facet<'a>>(value: &'a T) -> Result<String, SerializeError> {
    let mut document = serialize_top_level(Peek::new(value).innermost_peek())?;
    document.autoformat();
    Ok(document.to_string())
}

/// Error type for KDL serialization.
#[derive(Debug, Clone)]
pub enum SerializeError {
    /// A type error currently explained through the reflection system.
    ReflectError(ReflectError),
    /// The top level shape may only contain children, not arguments, parameters, or node_names.
    IllegalAttributesOnTopLevelShape,
    /// Currently we only support up to i128::MAX.
    U128TooLarge,
    /// Each variant may have at most one name.
    DuplicateNodeName,
    /// We don't know how to serialize this type.
    UnsupportedType,
}

impl From<ReflectError> for SerializeError {
    fn from(value: ReflectError) -> Self {
        SerializeError::ReflectError(value)
    }
}

impl fmt::Display for SerializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> fmt::Result {
        // TODO: Better formatting
        match self {
            SerializeError::ReflectError(e) => fmt::Display::fmt(e, f),
            _ => fmt::Debug::fmt(self, f),
        }
    }
}
impl Error for SerializeError {}

fn serialize_top_level(peek: Peek) -> Result<KdlDocument, SerializeError> {
    let shape = peek.shape();

    let Ok(peek) = peek.into_struct() else {
        // TODO: It would be nice to have enum/union support here.
        return Err(ReflectError::WasNotA {
            expected: "struct",
            actual: shape,
        }
        .into());
    };

    if peek.ty().fields.iter().any(|f| {
        f.attributes
            .contains(&FieldAttribute::Arbitrary("node_name"))
            || f.attributes
                .contains(&FieldAttribute::Arbitrary("argument"))
            || f.attributes
                .contains(&FieldAttribute::Arbitrary("arguments"))
            || f.attributes
                .contains(&FieldAttribute::Arbitrary("parameter"))
    }) {
        // Top level struct can't contain arguments or a node_name field, only children
        return Err(SerializeError::IllegalAttributesOnTopLevelShape);
    };

    serialize_children(peek).map(|opt| opt.unwrap_or_default())
}

fn serialize_children(peek: PeekStruct) -> Result<Option<KdlDocument>, SerializeError> {
    let mut document = KdlDocument::new();
    let mut empty = true;

    for (i, field) in peek.ty().fields.iter().enumerate() {
        if field.flags.contains(FieldFlags::CHILD) {
            empty = false;
            document
                .nodes_mut()
                // TODO: Assert that serialize_child uses the node name?
                .push(serialize_child(
                    field.name,
                    peek.field(i).unwrap().innermost_peek(),
                )?)
        }
    }

    let mut has_children = false;
    for (i, field) in peek.ty().fields.iter().enumerate() {
        if field
            .attributes
            .contains(&FieldAttribute::Arbitrary("children"))
        {
            empty = false;
            let Ok(peek) = peek.field(i).unwrap().innermost_peek().into_list_like() else {
                todo!("Probably need to handle sets and maps as well, and otherwise error")
            };

            for child in peek.iter() {
                document
                    .nodes_mut()
                    .push(serialize_child(field.name, child.innermost_peek())?)
            }
        }
    }

    if empty { Ok(None) } else { Ok(Some(document)) }
}

fn serialize_child(name: &'static str, peek: Peek) -> Result<KdlNode, SerializeError> {
    let mut res = KdlNode::new(name);

    match peek.shape().ty {
        Type::Primitive(_) => res.push(serialize_value(peek)?),
        Type::Sequence(sequence_type) => {
            // TODO: Should propose the use of the children attribute
            return Err(ReflectError::WasNotA {
                expected: "Serializable non-sequence type",
                actual: peek.shape(),
            }
            .into());
        }
        // Unsupported pointer, innermost_peek handles the ones we support before we get here.
        Type::Pointer(_) => return Err(SerializeError::UnsupportedType),
        // Opaque types would need a new attribute
        Type::User(UserType::Opaque) => return Err(SerializeError::UnsupportedType),
        Type::User(UserType::Enum(_)) | Type::User(UserType::Union(_)) => todo!(),
        Type::User(UserType::Struct(_)) => {
            let peek = peek.into_struct().unwrap();
            let fields_iter = peek.ty().fields.iter().enumerate();

            let mut found_node_name = false;
            for (i, field) in fields_iter.clone() {
                // Defer evaluation until we actual use it so that we
                // don't try to get fields that we aren't serializing.
                //
                // I think this might be necessary to avoid FieldError::Unsized?
                let peek_field = || peek.field(i).unwrap().innermost_peek();

                if field
                    .attributes
                    .contains(&FieldAttribute::Arbitrary("node_name"))
                {
                    if found_node_name {
                        return Err(SerializeError::DuplicateNodeName);
                    }
                    found_node_name = true;
                    let name = peek_field().to_string();
                    res.set_name(name);
                } else if field
                    .attributes
                    .contains(&FieldAttribute::Arbitrary("argument"))
                {
                    res.push(serialize_value(peek_field())?)
                } else if field
                    .attributes
                    .contains(&FieldAttribute::Arbitrary("arguments"))
                {
                    // TODO: Should I detect multiple "arguments" lists and "argument"
                    // after "arguments"? Deserializing in those cases is hard to impossible...
                    let list = peek_field().into_list_like()?;

                    for item in list.iter() {
                        let item = item.innermost_peek();
                        res.push(serialize_value(item)?);
                    }
                } else if field
                    .attributes
                    .contains(&FieldAttribute::Arbitrary("property"))
                {
                    let value = serialize_value(peek_field())?;
                    res.push(KdlEntry::new_prop(field.name, value));
                }
                // TODO: Should I warn for unused fields (also checking child/children)
            }

            if let Some(children) = serialize_children(peek)? {
                res.set_children(children);
            }
        }
    }

    Ok(res)
}

fn serialize_value(peek: Peek) -> Result<KdlValue, SerializeError> {
    // Strings aren't primitive but are treated as such
    if let Ok(s) = peek.get::<String>() {
        return Ok(s.as_str().into());
    }
    if let Ok(s) = peek.get::<Cow<str>>() {
        return Ok(s.as_ref().into());
    }

    let Type::Primitive(ty) = peek.shape().ty else {
        return Err(ReflectError::WasNotA {
            expected: "primitive value type",
            actual: peek.shape(),
        }
        .into());
    };

    Ok(match ty {
        PrimitiveType::Boolean => (*peek.get::<bool>()?).into(),
        PrimitiveType::Never => unreachable!(),
        PrimitiveType::Numeric(NumericType::Integer { .. }) => {
            // i128 overflow. Unfortunate consequence of using kdl-rs library.
            into_i128(peek)?.into()
        }
        PrimitiveType::Numeric(NumericType::Float) => into_f64(peek).into(),
        PrimitiveType::Textual(t) => peek.to_string().into(),
    })
}

fn into_i128(peek: Peek) -> Result<i128, SerializeError> {
    Ok(match peek.scalar_type() {
        Some(ScalarType::I8) => *peek.get::<i8>().unwrap() as i128,
        Some(ScalarType::I16) => *peek.get::<i16>().unwrap() as i128,
        Some(ScalarType::I32) => *peek.get::<i32>().unwrap() as i128,
        Some(ScalarType::I64) => *peek.get::<i64>().unwrap() as i128,
        Some(ScalarType::I128) => *peek.get::<i128>().unwrap(),

        Some(ScalarType::U8) => *peek.get::<u8>().unwrap() as i128,
        Some(ScalarType::U16) => *peek.get::<u16>().unwrap() as i128,
        Some(ScalarType::U32) => *peek.get::<u32>().unwrap() as i128,
        Some(ScalarType::U64) => *peek.get::<u64>().unwrap() as i128,
        // u128 may overflow
        Some(ScalarType::U128) => {
            return (*peek.get::<u128>().unwrap())
                .try_into()
                .map_err(|_| SerializeError::U128TooLarge);
        }

        Some(_) | None => unreachable!("into_i128 called on non integer type?"),
    })
}

fn into_f64(peek: Peek) -> f64 {
    match peek.scalar_type() {
        Some(ScalarType::F32) => *peek.get::<f32>().unwrap() as f64,
        Some(ScalarType::F64) => *peek.get::<f64>().unwrap(),

        Some(_) | None => unreachable!("into_f64 called on non float type?"),
    }
}
