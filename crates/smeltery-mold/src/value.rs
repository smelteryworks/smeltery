//! [`Value`], the data a template is rendered against in runtime mode, and [`to_value`].

use serde::ser::{self, Serialize};
use std::fmt;

/// A dynamic value: what template data becomes in runtime mode.
///
/// Built from any `T: Serialize` with [`to_value`]. Maps keep their insertion order, so a struct's fields and a
/// `BTreeMap`'s entries come out in the order Rust iterates them.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub enum Value {
    /// `None`, `()` or a missing optional value. Displays as nothing.
    #[default]
    Null,
    /// A boolean.
    Bool(bool),
    /// A signed integer (every integer that fits in `i64`).
    Int(i64),
    /// An unsigned integer above `i64::MAX`.
    UInt(u64),
    /// A float. `f32` values are converted through their shortest decimal form, so they display exactly as Rust's
    /// `f32` `Display` does.
    Float(f64),
    /// A string (escaped when echoed with `{{ }}`).
    Str(String),
    /// A list.
    List(Vec<Value>),
    /// A map with string keys, in insertion order.
    Map(Vec<(String, Value)>),
    /// Already-safe HTML (component slots); never escaped.
    Safe(String),
}

impl Value {
    /// Looks up `key` in a map value.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// A short name of the value's kind, for error messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "a boolean",
            Value::Int(_) | Value::UInt(_) => "an integer",
            Value::Float(_) => "a float",
            Value::Str(_) | Value::Safe(_) => "a string",
            Value::List(_) => "a list",
            Value::Map(_) => "a map",
        }
    }
}

impl Serialize for Value {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use ser::{SerializeMap, SerializeSeq};
        match self {
            Value::Null => s.serialize_none(),
            Value::Bool(b) => s.serialize_bool(*b),
            Value::Int(i) => s.serialize_i64(*i),
            Value::UInt(u) => s.serialize_u64(*u),
            Value::Float(f) => s.serialize_f64(*f),
            Value::Str(v) | Value::Safe(v) => s.serialize_str(v),
            Value::List(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Value::Map(entries) => {
                let mut map = s.serialize_map(Some(entries.len()))?;
                for (k, v) in entries {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

/// Why a value could not be converted with [`to_value`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot convert template data: {0}")]
pub struct ValueError(String);

impl ser::Error for ValueError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        ValueError(msg.to_string())
    }
}

/// Converts any `T: Serialize` into a [`Value`].
///
/// ```
/// use smeltery_mold::{to_value, Value};
/// assert_eq!(to_value(&Some(3u8)).unwrap(), Value::Int(3));
/// assert_eq!(to_value(&0.1f32).unwrap().to_string(), "0.1");
/// ```
pub fn to_value<T: Serialize + ?Sized>(value: &T) -> Result<Value, ValueError> {
    value.serialize(ValueSerializer)
}

impl fmt::Display for Value {
    /// The display form used by `{{ }}` (without escaping); lists and maps display as nothing.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null | Value::List(_) | Value::Map(_) => Ok(()),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::UInt(u) => write!(f, "{u}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Str(s) | Value::Safe(s) => f.write_str(s),
        }
    }
}

/// `f32` through its shortest decimal form, so the `f64` displays exactly like the `f32` did.
pub(crate) fn f32_to_f64(x: f32) -> f64 {
    if x.is_finite() {
        x.to_string().parse().unwrap_or(f64::from(x))
    } else {
        f64::from(x)
    }
}

fn int_value(i: i128) -> Result<Value, ValueError> {
    if let Ok(v) = i64::try_from(i) {
        Ok(Value::Int(v))
    } else if let Ok(v) = u64::try_from(i) {
        Ok(Value::UInt(v))
    } else {
        Err(ValueError(format!("integer {i} is out of range")))
    }
}

struct ValueSerializer;

/// Collects a sequence.
#[doc(hidden)]
pub struct SeqSer(Vec<Value>);
/// Collects a map or struct.
#[doc(hidden)]
pub struct MapSer {
    entries: Vec<(String, Value)>,
    key: Option<String>,
}
/// Collects an enum variant's fields as `{variant: …}`.
#[doc(hidden)]
pub struct VariantSer {
    name: &'static str,
    inner: VariantInner,
}
enum VariantInner {
    Seq(Vec<Value>),
    Map(Vec<(String, Value)>),
}

impl ser::Serializer for ValueSerializer {
    type Ok = Value;
    type Error = ValueError;
    type SerializeSeq = SeqSer;
    type SerializeTuple = SeqSer;
    type SerializeTupleStruct = SeqSer;
    type SerializeTupleVariant = VariantSer;
    type SerializeMap = MapSer;
    type SerializeStruct = MapSer;
    type SerializeStructVariant = VariantSer;

    fn serialize_bool(self, v: bool) -> Result<Value, ValueError> {
        Ok(Value::Bool(v))
    }
    fn serialize_i8(self, v: i8) -> Result<Value, ValueError> {
        Ok(Value::Int(v.into()))
    }
    fn serialize_i16(self, v: i16) -> Result<Value, ValueError> {
        Ok(Value::Int(v.into()))
    }
    fn serialize_i32(self, v: i32) -> Result<Value, ValueError> {
        Ok(Value::Int(v.into()))
    }
    fn serialize_i64(self, v: i64) -> Result<Value, ValueError> {
        Ok(Value::Int(v))
    }
    fn serialize_i128(self, v: i128) -> Result<Value, ValueError> {
        int_value(v)
    }
    fn serialize_u8(self, v: u8) -> Result<Value, ValueError> {
        Ok(Value::Int(v.into()))
    }
    fn serialize_u16(self, v: u16) -> Result<Value, ValueError> {
        Ok(Value::Int(v.into()))
    }
    fn serialize_u32(self, v: u32) -> Result<Value, ValueError> {
        Ok(Value::Int(v.into()))
    }
    fn serialize_u64(self, v: u64) -> Result<Value, ValueError> {
        int_value(v.into())
    }
    fn serialize_u128(self, v: u128) -> Result<Value, ValueError> {
        i128::try_from(v)
            .map_err(|_| ValueError(format!("integer {v} is out of range")))
            .and_then(int_value)
    }
    fn serialize_f32(self, v: f32) -> Result<Value, ValueError> {
        Ok(Value::Float(f32_to_f64(v)))
    }
    fn serialize_f64(self, v: f64) -> Result<Value, ValueError> {
        Ok(Value::Float(v))
    }
    fn serialize_char(self, v: char) -> Result<Value, ValueError> {
        Ok(Value::Str(v.to_string()))
    }
    fn serialize_str(self, v: &str) -> Result<Value, ValueError> {
        Ok(Value::Str(v.to_owned()))
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<Value, ValueError> {
        Ok(Value::List(
            v.iter().map(|b| Value::Int((*b).into())).collect(),
        ))
    }
    fn serialize_none(self) -> Result<Value, ValueError> {
        Ok(Value::Null)
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Value, ValueError> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<Value, ValueError> {
        Ok(Value::Null)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Value, ValueError> {
        Ok(Value::Null)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<Value, ValueError> {
        Ok(Value::Str(variant.to_owned()))
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<Value, ValueError> {
        match value.serialize(self)? {
            // `rt::Safe` (slot HTML) stays unescaped, as it does in compiled templates.
            Value::Str(s) if name == crate::rt::SAFE_NAME => Ok(Value::Safe(s)),
            v => Ok(v),
        }
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Value, ValueError> {
        Ok(Value::Map(vec![(variant.to_owned(), to_value(value)?)]))
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<SeqSer, ValueError> {
        Ok(SeqSer(Vec::with_capacity(len.unwrap_or(0))))
    }
    fn serialize_tuple(self, len: usize) -> Result<SeqSer, ValueError> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(self, _: &'static str, len: usize) -> Result<SeqSer, ValueError> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<VariantSer, ValueError> {
        Ok(VariantSer {
            name: variant,
            inner: VariantInner::Seq(Vec::new()),
        })
    }
    fn serialize_map(self, _: Option<usize>) -> Result<MapSer, ValueError> {
        Ok(MapSer {
            entries: Vec::new(),
            key: None,
        })
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<MapSer, ValueError> {
        self.serialize_map(None)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<VariantSer, ValueError> {
        Ok(VariantSer {
            name: variant,
            inner: VariantInner::Map(Vec::new()),
        })
    }
}

impl ser::SerializeSeq for SeqSer {
    type Ok = Value;
    type Error = ValueError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), ValueError> {
        self.0.push(to_value(value)?);
        Ok(())
    }
    fn end(self) -> Result<Value, ValueError> {
        Ok(Value::List(self.0))
    }
}

impl ser::SerializeTuple for SeqSer {
    type Ok = Value;
    type Error = ValueError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), ValueError> {
        ser::SerializeSeq::serialize_element(self, value)
    }
    fn end(self) -> Result<Value, ValueError> {
        ser::SerializeSeq::end(self)
    }
}

impl ser::SerializeTupleStruct for SeqSer {
    type Ok = Value;
    type Error = ValueError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), ValueError> {
        ser::SerializeSeq::serialize_element(self, value)
    }
    fn end(self) -> Result<Value, ValueError> {
        ser::SerializeSeq::end(self)
    }
}

impl ser::SerializeMap for MapSer {
    type Ok = Value;
    type Error = ValueError;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), ValueError> {
        let key = match to_value(key)? {
            Value::Str(s) => s,
            v @ (Value::Int(_) | Value::UInt(_) | Value::Bool(_) | Value::Float(_)) => {
                v.to_string()
            }
            other => {
                return Err(ValueError(format!(
                    "map keys must be strings or numbers, not {}",
                    other.kind()
                )));
            }
        };
        self.key = Some(key);
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), ValueError> {
        let key = self
            .key
            .take()
            .ok_or_else(|| ValueError("map value without a key".to_owned()))?;
        self.entries.push((key, to_value(value)?));
        Ok(())
    }
    fn end(self) -> Result<Value, ValueError> {
        Ok(Value::Map(self.entries))
    }
}

impl ser::SerializeStruct for MapSer {
    type Ok = Value;
    type Error = ValueError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), ValueError> {
        self.entries.push((key.to_owned(), to_value(value)?));
        Ok(())
    }
    fn end(self) -> Result<Value, ValueError> {
        Ok(Value::Map(self.entries))
    }
}

impl ser::SerializeTupleVariant for VariantSer {
    type Ok = Value;
    type Error = ValueError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), ValueError> {
        if let VariantInner::Seq(items) = &mut self.inner {
            items.push(to_value(value)?);
        }
        Ok(())
    }
    fn end(self) -> Result<Value, ValueError> {
        Ok(self.finish())
    }
}

impl ser::SerializeStructVariant for VariantSer {
    type Ok = Value;
    type Error = ValueError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), ValueError> {
        if let VariantInner::Map(entries) = &mut self.inner {
            entries.push((key.to_owned(), to_value(value)?));
        }
        Ok(())
    }
    fn end(self) -> Result<Value, ValueError> {
        Ok(self.finish())
    }
}

impl VariantSer {
    fn finish(self) -> Value {
        let inner = match self.inner {
            VariantInner::Seq(items) => Value::List(items),
            VariantInner::Map(entries) => Value::Map(entries),
        };
        Value::Map(vec![(self.name.to_owned(), inner)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(serde::Serialize)]
    struct Post {
        title: String,
        views: u64,
        score: f32,
        tags: Vec<&'static str>,
        author: Option<String>,
    }

    #[derive(serde::Serialize)]
    enum Kind {
        A,
        B(i32),
        C { x: i32 },
    }

    #[test]
    fn struct_to_map_in_field_order() {
        let p = Post {
            title: "Hi".into(),
            views: u64::MAX,
            score: 0.3,
            tags: vec!["a"],
            author: None,
        };
        let v = to_value(&p).unwrap();
        let Value::Map(entries) = &v else {
            panic!("not a map")
        };
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["title", "views", "score", "tags", "author"]);
        assert_eq!(v.get("views"), Some(&Value::UInt(u64::MAX)));
        assert_eq!(v.get("score").unwrap().to_string(), "0.3");
        assert_eq!(v.get("author"), Some(&Value::Null));
    }

    #[test]
    fn enums_maps_and_errors() {
        assert_eq!(to_value(&Kind::A).unwrap(), Value::Str("A".into()));
        assert_eq!(
            to_value(&Kind::B(1)).unwrap(),
            Value::Map(vec![("B".into(), Value::Int(1))])
        );
        assert_eq!(
            to_value(&Kind::C { x: 2 }).unwrap(),
            Value::Map(vec![(
                "C".into(),
                Value::Map(vec![("x".into(), Value::Int(2))])
            )])
        );
        let mut m = BTreeMap::new();
        m.insert(2, "b");
        m.insert(1, "a");
        assert_eq!(
            to_value(&m).unwrap(),
            Value::Map(vec![
                ("1".into(), Value::Str("a".into())),
                ("2".into(), Value::Str("b".into()))
            ])
        );
        let mut bad = BTreeMap::new();
        bad.insert(vec![1], 1);
        assert!(to_value(&bad).is_err());
        assert!(to_value(&i128::MAX).is_err());
        assert_eq!(to_value(&(-5i128)).unwrap(), Value::Int(-5));
    }
}
