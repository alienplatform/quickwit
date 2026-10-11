// Copyright 2021-Present Datadog, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::cmp::Ordering;
use std::fmt;
use std::io::{self, Read};

use prost::Message;
pub use sort_by_value::SortValue;

include!("../codegen/quickwit/quickwit.search.rs");

pub const SEARCH_FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!("../codegen/quickwit/search_descriptor.bin");

impl SearchRequest {
    pub fn time_range(&self) -> impl std::ops::RangeBounds<i64> {
        use std::ops::Bound;
        (
            self.start_timestamp
                .map_or(Bound::Unbounded, Bound::Included),
            self.end_timestamp.map_or(Bound::Unbounded, Bound::Excluded),
        )
    }
}

impl SplitIdAndFooterOffsets {
    pub fn time_range(&self) -> impl std::ops::RangeBounds<i64> {
        use std::ops::Bound;
        (
            self.timestamp_start
                .map_or(Bound::Unbounded, Bound::Included),
            self.timestamp_end.map_or(Bound::Unbounded, Bound::Included),
        )
    }
}

impl fmt::Display for SplitSearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, split_id: {})", self.error, self.split_id)
    }
}

impl Eq for SortByValue {}

impl From<SortValue> for SortByValue {
    fn from(sort_value: SortValue) -> Self {
        SortByValue {
            sort_value: Some(sort_value),
        }
    }
}

impl std::hash::Hash for SortByValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.sort_value.hash(state);
    }
}

impl SortByValue {
    pub fn into_json(self) -> serde_json::Value {
        use serde_json::Value::*;
        match self.sort_value {
            Some(SortValue::U64(num)) => Number(num.into()),
            Some(SortValue::I64(num)) => Number(num.into()),
            Some(SortValue::F64(num)) => {
                if let Some(num) = serde_json::Number::from_f64(num) {
                    Number(num)
                } else {
                    // TODO is there a better way to handle infinite/nan?
                    Null
                }
            }
            Some(SortValue::Boolean(b)) => Bool(b),
            None => Null,
        }
    }

    pub fn try_from_json(value: serde_json::Value) -> Option<Self> {
        use serde_json::Value::*;
        let sort_value = match value {
            Null => None,
            Bool(b) => Some(SortValue::Boolean(b)),
            Number(number) => {
                if let Some(number) = number.as_u64() {
                    Some(SortValue::U64(number))
                } else if let Some(number) = number.as_i64() {
                    Some(SortValue::I64(number))
                } else if let Some(number) = number.as_f64() {
                    Some(SortValue::F64(number))
                } else {
                    // this should never happen as we don't emit such number ourselves
                    return None;
                }
            }
            // Strings that can be converted to a number are accepted.
            // Some clients (like JS clients) can't easily handle large integers
            // without losing precision, so we accept them as strings.
            String(value) => {
                if let Ok(number) = value.parse::<i64>() {
                    Some(SortValue::I64(number))
                } else if let Ok(number) = value.parse::<u64>() {
                    Some(SortValue::U64(number))
                } else if value.contains(['.', 'e', 'E']) {
                    // An overflowing integer string must not silently round into
                    // a float. Floating cursor encodings identify their domain.
                    let number = value.parse::<f64>().ok()?;
                    if !number.is_finite() {
                        return None;
                    }
                    Some(SortValue::F64(number))
                } else {
                    return None;
                }
            }
            Array(_) | Object(_) => return None,
        };
        Some(SortByValue { sort_value })
    }
}

// !!! Disclaimer !!!
//
// Prost imposes the PartialEq derived implementation.
// This is terrible because this means Eq, PartialEq are not really in line with Ord's
// implementation. if in presence of NaN.
impl Eq for SortValue {}

// Casting the integer to f64 first makes equality non-transitive above 2^53.
// All supported integers fit in i128; saturating float conversion also orders
// infinities and finite out-of-range values outside the integer domain.
fn compare_float_integer(left: f64, right: i128) -> Ordering {
    if left.is_nan() {
        return left.total_cmp(&(right as f64));
    }
    let truncated = left as i128;
    truncated
        .cmp(&right)
        .then_with(|| left.total_cmp(&(truncated as f64)))
}

impl Ord for SortValue {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        // We make sure to end up with a total order.
        match (*self, *other) {
            // Same types.
            (SortValue::U64(left), SortValue::U64(right)) => left.cmp(&right),
            (SortValue::I64(left), SortValue::I64(right)) => left.cmp(&right),
            (SortValue::Boolean(left), SortValue::Boolean(right)) => left.cmp(&right),
            // We half the logic by making sure we keep
            // the "stronger" type on the left.
            (SortValue::U64(left), SortValue::I64(right)) => {
                if left > i64::MAX as u64 {
                    return Ordering::Greater;
                }
                (left as i64).cmp(&right)
            }
            (SortValue::F64(left), SortValue::F64(right)) => left.total_cmp(&right),
            (SortValue::F64(left), SortValue::U64(right)) => {
                compare_float_integer(left, right as i128)
            }
            (SortValue::F64(left), SortValue::I64(right)) => {
                compare_float_integer(left, right as i128)
            }
            (SortValue::Boolean(left), right) => SortValue::U64(left as u64).cmp(&right),
            (left, right) => right.cmp(&left).reverse(),
        }
    }
}

impl PartialOrd for SortValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::hash::Hash for SortValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        let this = self.normalize();
        std::mem::discriminant(&this).hash(state);
        match this {
            SortValue::U64(number) => {
                number.hash(state);
            }
            SortValue::I64(number) => {
                number.hash(state);
            }
            SortValue::F64(number) => {
                number.to_bits().hash(state);
            }
            SortValue::Boolean(b) => {
                b.hash(state);
            }
        }
    }
}

impl SortValue {
    /// Where multiple variant could represent the same logical value, convert to a canonical form.
    ///
    /// For number, we prefer to represent them, in order, as i64, then as u64 and finally as f64.
    pub fn normalize(&self) -> Self {
        match self {
            SortValue::I64(_) => *self,
            SortValue::Boolean(_) => *self,
            SortValue::U64(number) => {
                if let Ok(number) = (*number).try_into() {
                    SortValue::I64(number)
                } else {
                    *self
                }
            }
            SortValue::F64(number) => {
                let number = *number;
                if number.ceil() == number {
                    // number is not NaN, and is a natural number
                    if number >= i64::MIN as f64 && number <= i64::MAX as f64 {
                        return SortValue::I64(number as i64);
                    } else if number.is_sign_positive() && number <= u64::MAX as f64 {
                        return SortValue::U64(number as u64);
                    }
                }
                *self
            }
        }
    }
}

impl PartialHit {
    /// Helper to get access to the 1st sort value
    pub fn sort_value(&self) -> Option<SortValue> {
        if let Some(sort_value) = self.sort_value {
            sort_value.sort_value
        } else {
            None
        }
    }
}

/// Serializes the Split fields.
///
/// `fields_metadata` has to be sorted.
pub fn serialize_split_fields(list_fields: ListFields) -> Vec<u8> {
    let payload = list_fields.encode_to_vec();
    let compression_level = 3;
    let payload_compressed = zstd::stream::encode_all(&mut &payload[..], compression_level)
        .expect("zstd encoding failed");
    let mut out = Vec::new();
    // Write Header -- Format Version 2
    let format_version = 2u8;
    out.push(format_version);
    // Write Payload
    out.extend_from_slice(&payload_compressed);
    out
}

/// Reads a fixed number of bytes into an array and returns the array.
fn read_exact_array<const N: usize>(reader: &mut impl Read) -> io::Result<[u8; N]> {
    let mut buffer = [0u8; N];
    reader.read_exact(&mut buffer)?;
    Ok(buffer)
}

/// Reads the Split fields from a zstd compressed stream of bytes
pub fn deserialize_split_fields<R: Read>(mut reader: R) -> io::Result<ListFields> {
    let format_version = read_exact_array::<1>(&mut reader)?[0];
    if format_version != 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Unsupported split field format version: {format_version}"),
        ));
    }
    let reader = zstd::Decoder::new(reader)?;
    read_split_fields_from_zstd(reader)
}

/// Reads the Split fields from a stream of bytes
#[allow(clippy::unbuffered_bytes)]
fn read_split_fields_from_zstd<R: Read>(reader: R) -> io::Result<ListFields> {
    let all_bytes: Vec<_> = reader.bytes().collect::<io::Result<_>>()?;
    let serialized_list_fields: ListFields = prost::Message::decode(&all_bytes[..])?;

    Ok(serialized_list_fields)
}

#[cfg(test)]
mod numeric_sort_tests {
    use std::cmp::Ordering;

    use super::{SortByValue, SortValue};

    #[test]
    fn numeric_cursor_strings_preserve_float_sign_and_reject_nonfinite_values() {
        for value in [-0.0f64, 0.0, -1.5, f64::MIN_POSITIVE, f64::MAX] {
            let decoded =
                SortByValue::try_from_json(serde_json::json!(format!("{value:?}"))).unwrap();
            let Some(SortValue::F64(actual)) = decoded.sort_value else {
                panic!("floating cursor must retain its numeric domain");
            };
            assert_eq!(actual.to_bits(), value.to_bits());
        }
        for value in [
            "NaN",
            "inf",
            "-inf",
            "1e999",
            "not-a-number",
            "18446744073709551616",
            "-9223372036854775809",
        ] {
            assert!(SortByValue::try_from_json(serde_json::json!(value)).is_none());
        }
    }

    #[test]
    fn mixed_numeric_order_preserves_integer_precision_and_transitivity() {
        use SortValue::{F64, I64, U64};
        let ordered = [
            F64(f64::NEG_INFINITY),
            I64(i64::MIN),
            F64(-1.5),
            I64(-1),
            F64(-0.0),
            U64(0),
            F64(0.5),
            U64(1),
            F64(9_007_199_254_740_992.0),
            U64(9_007_199_254_740_993),
            F64(9_007_199_254_740_994.0),
            U64(9_007_199_254_740_995),
            I64(i64::MAX),
            F64(9_223_372_036_854_775_808.0),
            U64(u64::MAX),
            F64(18_446_744_073_709_551_616.0),
            F64(f64::INFINITY),
        ];
        for (index, left) in ordered.iter().enumerate() {
            for right in &ordered[index + 1..] {
                assert_eq!(left.cmp(right), Ordering::Less, "{left:?} < {right:?}");
                assert_eq!(right.cmp(left), Ordering::Greater);
            }
        }
        assert_eq!(F64(0.0).cmp(&U64(0)), Ordering::Equal);
        assert_eq!(F64(i64::MIN as f64).cmp(&I64(i64::MIN)), Ordering::Equal);
        assert_eq!(F64(f64::NAN).cmp(&U64(u64::MAX)), Ordering::Greater);
    }
}
