// ABOUTME: A serde `Serializer` that walks a section frame and records, for every
// node, its key path, the struct field that holds it and the field's Rust type.
//
// JSON has no types: a `PathBuf`, a scrollback capture and a session name are
// all strings once serialised. The type deny-list (#983 check 2) needs the
// declared type, and serde hands it over for free: every value reaches a
// serializer through a generic `serialize_field<T>` / `serialize_element<T>` /
// `serialize_some<T>` call, where `std::any::type_name::<T>()` names it. The
// tracer serialises nothing itself; it walks the exact values `section_json`
// would emit, so what it reports is what a host receives.
//
// Path grammar, rooted at the section's wire name:
//   `.field`       struct field, or an externally tagged enum variant name
//   `[]`           a sequence or tuple element (indices are data, not shape)
//   `{}`           a map value (keys are data; they are recorded separately)

use serde::ser::{self, Serialize};
use std::collections::BTreeSet;
use std::fmt;

/// One struct (or struct-variant) field reached while serialising a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldNode {
    /// Normalised key path, e.g. `config.app_config.fleet.bridge`.
    pub path: String,
    /// `Owner.field`, the stable name allow-lists key on. The owner is the
    /// serde struct name, or `Enum::Variant` for a struct variant.
    pub owner_field: String,
    /// `std::any::type_name` of the declared field type, references stripped.
    pub rust_type: String,
}

/// One string that reaches the frame, with the keys above it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringLeaf {
    pub path: String,
    /// The innermost `Owner.field` above the string.
    pub owner_field: String,
    /// Every JSON key above the string, map keys included, outermost first.
    pub keys: Vec<KeySegment>,
    pub value: String,
}

/// A JSON key on the way down to a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySegment {
    pub key: String,
    /// `Owner.field` for a struct field, `Owner.field{}` for a key of the map
    /// that field holds.
    pub owner_field: String,
}

/// Everything a trace of one or more frames found.
#[derive(Debug, Default, Clone)]
pub struct Trace {
    pub fields: Vec<FieldNode>,
    pub strings: Vec<StringLeaf>,
    /// Paths where a scalar, a `null` or an empty container was emitted.
    pub leaf_paths: BTreeSet<String>,
}

/// Walk `value` as if serialising it, rooted at `root`.
#[must_use]
pub fn trace<T: Serialize + ?Sized>(root: &str, value: &T) -> Trace {
    let mut out = Trace::default();
    let cx = Cx {
        path: root.to_string(),
        owner_field: String::new(),
        keys: Vec::new(),
    };
    // The tracer's only error is a custom one raised by a value's own
    // `Serialize`; the partial trace is still the best record available.
    let _ = value.serialize(Tracer { out: &mut out, cx });
    out
}

/// Strip the reference and serde wrapper noise from a type name.
fn clean_type<T: ?Sized>() -> String {
    let mut name = std::any::type_name::<T>();
    while let Some(rest) = name.strip_prefix('&') {
        name = rest.trim_start_matches("mut ");
    }
    name.to_string()
}

#[derive(Debug)]
pub struct TraceError(String);

impl fmt::Display for TraceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TraceError {}

impl ser::Error for TraceError {
    fn custom<M: fmt::Display>(msg: M) -> Self {
        Self(msg.to_string())
    }
}

#[derive(Clone)]
struct Cx {
    path: String,
    owner_field: String,
    keys: Vec<KeySegment>,
}

impl Cx {
    fn field(&self, owner: &str, key: &str) -> Self {
        let owner_field = format!("{owner}.{key}");
        let mut keys = self.keys.clone();
        keys.push(KeySegment {
            key: key.to_string(),
            owner_field: owner_field.clone(),
        });
        Self {
            path: format!("{}.{key}", self.path),
            owner_field,
            keys,
        }
    }

    fn element(&self) -> Self {
        Self {
            path: format!("{}[]", self.path),
            ..self.clone()
        }
    }

    fn map_value(&self, key: Option<String>) -> Self {
        let mut keys = self.keys.clone();
        if let Some(key) = key {
            keys.push(KeySegment {
                key,
                owner_field: format!("{}{{}}", self.owner_field),
            });
        }
        Self {
            path: format!("{}{{}}", self.path),
            owner_field: self.owner_field.clone(),
            keys,
        }
    }
}

struct Tracer<'a> {
    out: &'a mut Trace,
    cx: Cx,
}

impl<'a> Tracer<'a> {
    // `Result` because every caller returns it straight from a `Serializer` method.
    #[allow(clippy::unnecessary_wraps)]
    fn leaf(self) -> Result<(), TraceError> {
        self.out.leaf_paths.insert(self.cx.path);
        Ok(())
    }

    fn string(self, value: &str) -> Result<(), TraceError> {
        self.out.strings.push(StringLeaf {
            path: self.cx.path.clone(),
            owner_field: self.cx.owner_field.clone(),
            keys: self.cx.keys.clone(),
            value: value.to_string(),
        });
        self.leaf()
    }

    fn record_field<T: ?Sized>(out: &mut Trace, cx: &Cx) {
        out.fields.push(FieldNode {
            path: cx.path.clone(),
            owner_field: cx.owner_field.clone(),
            rust_type: clean_type::<T>(),
        });
    }

    fn compound(self, owner: String) -> Compound<'a> {
        Compound {
            out: self.out,
            cx: self.cx,
            owner,
            pending_key: None,
            empty: true,
        }
    }
}

/// Serialises a map key to the string a JSON object would use.
struct KeyString;

macro_rules! key_display {
    ($($method:ident: $ty:ty),*) => {
        $(fn $method(self, v: $ty) -> Result<String, TraceError> { Ok(v.to_string()) })*
    };
}

impl ser::Serializer for KeyString {
    type Ok = String;
    type Error = TraceError;
    type SerializeSeq = ser::Impossible<String, TraceError>;
    type SerializeTuple = ser::Impossible<String, TraceError>;
    type SerializeTupleStruct = ser::Impossible<String, TraceError>;
    type SerializeTupleVariant = ser::Impossible<String, TraceError>;
    type SerializeMap = ser::Impossible<String, TraceError>;
    type SerializeStruct = ser::Impossible<String, TraceError>;
    type SerializeStructVariant = ser::Impossible<String, TraceError>;

    key_display!(serialize_bool: bool, serialize_i8: i8, serialize_i16: i16, serialize_i32: i32,
        serialize_i64: i64, serialize_u8: u8, serialize_u16: u16, serialize_u32: u32,
        serialize_u64: u64, serialize_f32: f32, serialize_f64: f64, serialize_char: char,
        serialize_str: &str);

    fn serialize_bytes(self, _: &[u8]) -> Result<String, TraceError> {
        Ok(String::new())
    }
    fn serialize_none(self) -> Result<String, TraceError> {
        Ok(String::new())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<String, TraceError> {
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<String, TraceError> {
        Ok(String::new())
    }
    fn serialize_unit_struct(self, name: &'static str) -> Result<String, TraceError> {
        Ok(name.to_string())
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<String, TraceError> {
        Ok(variant.to_string())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        v: &T,
    ) -> Result<String, TraceError> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: &T,
    ) -> Result<String, TraceError> {
        Ok(variant.to_string())
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, TraceError> {
        Err(ser::Error::custom("sequence map key"))
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, TraceError> {
        Err(ser::Error::custom("tuple map key"))
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, TraceError> {
        Err(ser::Error::custom("tuple struct map key"))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, TraceError> {
        Err(ser::Error::custom("tuple variant map key"))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, TraceError> {
        Err(ser::Error::custom("map map key"))
    }
    fn serialize_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStruct, TraceError> {
        Err(ser::Error::custom("struct map key"))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, TraceError> {
        Err(ser::Error::custom("struct variant map key"))
    }
}

macro_rules! scalar_leaf {
    ($($method:ident: $ty:ty),*) => {
        $(fn $method(self, _: $ty) -> Result<(), TraceError> { self.leaf() })*
    };
}

impl<'a> ser::Serializer for Tracer<'a> {
    type Ok = ();
    type Error = TraceError;
    type SerializeSeq = Compound<'a>;
    type SerializeTuple = Compound<'a>;
    type SerializeTupleStruct = Compound<'a>;
    type SerializeTupleVariant = Compound<'a>;
    type SerializeMap = Compound<'a>;
    type SerializeStruct = Compound<'a>;
    type SerializeStructVariant = Compound<'a>;

    scalar_leaf!(serialize_bool: bool, serialize_i8: i8, serialize_i16: i16, serialize_i32: i32,
        serialize_i64: i64, serialize_u8: u8, serialize_u16: u16, serialize_u32: u32,
        serialize_u64: u64, serialize_f32: f32, serialize_f64: f64, serialize_bytes: &[u8]);

    fn serialize_char(self, v: char) -> Result<(), TraceError> {
        self.string(&v.to_string())
    }
    fn serialize_str(self, v: &str) -> Result<(), TraceError> {
        self.string(v)
    }
    fn serialize_none(self) -> Result<(), TraceError> {
        self.leaf()
    }
    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<(), TraceError> {
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), TraceError> {
        self.leaf()
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), TraceError> {
        self.leaf()
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<(), TraceError> {
        // A unit variant is a string on the wire, but its content is the
        // variant name, fixed at compile time: it is a leaf, never a string a
        // secret could ride in.
        let _ = variant;
        self.leaf()
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        v: &T,
    ) -> Result<(), TraceError> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        _: u32,
        variant: &'static str,
        v: &T,
    ) -> Result<(), TraceError> {
        let owner_field = format!("{name}::{variant}.0");
        let mut keys = self.cx.keys.clone();
        keys.push(KeySegment {
            key: variant.to_string(),
            owner_field: owner_field.clone(),
        });
        let cx = Cx {
            path: format!("{}.{variant}", self.cx.path),
            owner_field,
            keys,
        };
        Tracer::record_field::<T>(self.out, &cx);
        v.serialize(Tracer { out: self.out, cx })
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Compound<'a>, TraceError> {
        Ok(self.compound(String::new()))
    }
    fn serialize_tuple(self, _: usize) -> Result<Compound<'a>, TraceError> {
        Ok(self.compound(String::new()))
    }
    fn serialize_tuple_struct(
        self,
        name: &'static str,
        _: usize,
    ) -> Result<Compound<'a>, TraceError> {
        Ok(self.compound(name.to_string()))
    }
    fn serialize_tuple_variant(
        mut self,
        name: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Compound<'a>, TraceError> {
        self.cx.path = format!("{}.{variant}", self.cx.path);
        self.cx.keys.push(KeySegment {
            key: variant.to_string(),
            owner_field: format!("{name}::{variant}"),
        });
        Ok(self.compound(format!("{name}::{variant}")))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Compound<'a>, TraceError> {
        Ok(self.compound(String::new()))
    }
    fn serialize_struct(self, name: &'static str, _: usize) -> Result<Compound<'a>, TraceError> {
        Ok(self.compound(name.to_string()))
    }
    fn serialize_struct_variant(
        mut self,
        name: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Compound<'a>, TraceError> {
        self.cx.path = format!("{}.{variant}", self.cx.path);
        self.cx.keys.push(KeySegment {
            key: variant.to_string(),
            owner_field: format!("{name}::{variant}"),
        });
        Ok(self.compound(format!("{name}::{variant}")))
    }
}

struct Compound<'a> {
    out: &'a mut Trace,
    cx: Cx,
    owner: String,
    pending_key: Option<String>,
    empty: bool,
}

impl Compound<'_> {
    fn element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), TraceError> {
        self.empty = false;
        v.serialize(Tracer {
            out: self.out,
            cx: self.cx.element(),
        })
    }

    fn field<T: Serialize + ?Sized>(&mut self, key: &str, v: &T) -> Result<(), TraceError> {
        self.empty = false;
        let cx = self.cx.field(&self.owner, key);
        Tracer::record_field::<T>(self.out, &cx);
        v.serialize(Tracer { out: self.out, cx })
    }

    // `Result` because every caller returns it straight from an `end` method.
    #[allow(clippy::unnecessary_wraps)]
    fn finish(self) -> Result<(), TraceError> {
        if self.empty {
            self.out.leaf_paths.insert(self.cx.path);
        }
        Ok(())
    }
}

impl ser::SerializeSeq for Compound<'_> {
    type Ok = ();
    type Error = TraceError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), TraceError> {
        self.element(v)
    }
    fn end(self) -> Result<(), TraceError> {
        self.finish()
    }
}

impl ser::SerializeTuple for Compound<'_> {
    type Ok = ();
    type Error = TraceError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), TraceError> {
        self.element(v)
    }
    fn end(self) -> Result<(), TraceError> {
        self.finish()
    }
}

impl ser::SerializeTupleStruct for Compound<'_> {
    type Ok = ();
    type Error = TraceError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), TraceError> {
        self.element(v)
    }
    fn end(self) -> Result<(), TraceError> {
        self.finish()
    }
}

impl ser::SerializeTupleVariant for Compound<'_> {
    type Ok = ();
    type Error = TraceError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), TraceError> {
        self.empty = false;
        let cx = Cx {
            path: format!("{}[]", self.cx.path),
            owner_field: format!("{}.0", self.owner),
            keys: self.cx.keys.clone(),
        };
        Tracer::record_field::<T>(self.out, &cx);
        v.serialize(Tracer { out: self.out, cx })
    }
    fn end(self) -> Result<(), TraceError> {
        self.finish()
    }
}

impl ser::SerializeMap for Compound<'_> {
    type Ok = ();
    type Error = TraceError;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), TraceError> {
        // A key JSON cannot render still has a place in the key chain, so the name
        // check sees that something unnamed is there instead of skipping it.
        self.pending_key =
            Some(key.serialize(KeyString).unwrap_or_else(|_| "<unrenderable>".to_string()));
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), TraceError> {
        self.empty = false;
        let cx = self.cx.map_value(self.pending_key.take());
        v.serialize(Tracer { out: self.out, cx })
    }
    fn end(self) -> Result<(), TraceError> {
        self.finish()
    }
}

impl ser::SerializeStruct for Compound<'_> {
    type Ok = ();
    type Error = TraceError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        v: &T,
    ) -> Result<(), TraceError> {
        self.field(key, v)
    }
    fn end(self) -> Result<(), TraceError> {
        self.finish()
    }
}

impl ser::SerializeStructVariant for Compound<'_> {
    type Ok = ();
    type Error = TraceError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        v: &T,
    ) -> Result<(), TraceError> {
        self.field(key, v)
    }
    fn end(self) -> Result<(), TraceError> {
        self.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[derive(Serialize)]
    struct Inner {
        path: PathBuf,
        env: HashMap<String, String>,
    }

    #[derive(Serialize)]
    enum Kind {
        Text { value: String },
        Unit,
    }

    #[derive(Serialize)]
    struct Outer {
        inner: Vec<Inner>,
        kind: Kind,
        unit: Kind,
        none: Option<u8>,
    }

    #[test]
    fn records_declared_types_paths_and_map_keys() {
        let value = Outer {
            inner: vec![Inner {
                path: PathBuf::from("/tmp/x"),
                env: HashMap::from([("TOKEN".to_string(), "v".to_string())]),
            }],
            kind: Kind::Text {
                value: "typed".to_string(),
            },
            unit: Kind::Unit,
            none: None,
        };
        let trace = trace("root", &value);

        let path_field = trace
            .fields
            .iter()
            .find(|f| f.owner_field == "Inner.path")
            .expect("Inner.path traced");
        assert_eq!(path_field.path, "root.inner[].path");
        assert_eq!(path_field.rust_type, "std::path::PathBuf");

        let env = trace.strings.iter().find(|s| s.value == "v").expect("env value traced");
        assert_eq!(env.path, "root.inner[].env{}");
        assert_eq!(env.keys.last().map(|k| k.key.as_str()), Some("TOKEN"));

        assert!(trace.fields.iter().any(|f| f.owner_field == "Kind::Text.value"));
        assert!(trace.leaf_paths.contains("root.kind.Text.value"));
        assert!(trace.leaf_paths.contains("root.unit"));
        assert!(trace.leaf_paths.contains("root.none"));
    }
}
