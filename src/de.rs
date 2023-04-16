use crate::nightly::utf8_char_width;
use crate::{Error, Result, E};
use serde::de::{
    DeserializeSeed, EnumAccess, IntoDeserializer, MapAccess, SeqAccess, VariantAccess, Visitor,
};
use serde::{Deserialize, Deserializer};

pub(crate) mod read;
use crate::int_code::NumericEncoding;
use read::{Read, ReadWith};

pub(crate) fn deserialize_with<'a, T: Deserialize<'a>, R: ReadWith<'a>>(
    bytes: &'a [u8],
    num_encoding: impl NumericEncoding,
) -> Result<T> {
    deserialize_from(R::from_inner(bytes), num_encoding)
}

pub(crate) fn deserialize_from<'a, T: Deserialize<'a>>(
    r: impl Read,
    num_encoding: impl NumericEncoding,
) -> Result<T> {
    let mut d = BitcodeDeserializer {
        data: r,
        num_encoding,
    };
    let result = T::deserialize(&mut d);

    let r = d.data.finish();
    if let Err(e) = &r {
        if e.same(&E::Eof.e()) {
            return Err(E::Eof.e());
        }
    }

    let t = result?;
    r?;
    Ok(t)
}

struct BitcodeDeserializer<R, N> {
    data: R,
    num_encoding: N,
}

macro_rules! read_int_encoding {
    ($name:ident, $a:ty) => {
        fn $name(&mut self) -> Result<$a> {
            self.num_encoding.decode(&mut self.data)
        }
    };
}

macro_rules! read_int_direct {
    ($name:ident, $a:ty) => {
        fn $name(&mut self) -> Result<$a> {
            self.data.read_bits(<$a>::BITS as usize).map(|v| v as $a)
        }
    };
}

impl<R: Read, N: NumericEncoding> BitcodeDeserializer<R, N> {
    read_int_encoding!(read_i8, i8);
    read_int_encoding!(read_i16, i16);
    read_int_direct!(read_i64, i64);
    read_int_encoding!(read_u8, u8);
    read_int_encoding!(read_u16, u16);
    read_int_encoding!(read_u32, u32);

    #[cfg(target_pointer_width = "64")]
    read_int_encoding!(read_i32, i32);
    // #[cfg(target_pointer_width = "64")]
    // read_int_encoding!(read_i64, i64);
    #[cfg(target_pointer_width = "64")]
    read_int_encoding!(read_u64, u64);

    #[cfg(not(target_pointer_width = "64"))]
    read_int_direct!(read_i32, i32);
    // #[cfg(not(target_pointer_width = "64"))]
    // read_int_direct!(read_i64, i64);
    #[cfg(not(target_pointer_width = "64"))]
    read_int_direct!(read_u64, u64);

    fn read_bool(&mut self) -> Result<bool> {
        self.data.read_bit()
    }

    fn read_len(&mut self) -> Result<usize> {
        self.num_encoding.decode_word(&mut self.data)
    }

    #[inline(never)] // Removing this makes bench_bitcode_deserialize 27% slower.
    fn read_len_and_bytes(&mut self) -> Result<Vec<u8>> {
        let len = self.read_len()?;
        if len > isize::MAX as usize / u8::MAX as usize {
            return Err(E::Invalid("length").e());
        }
        self.data.read_bytes(len)
    }

    fn read_variant_index(&mut self) -> Result<u32> {
        Ok(self
            .num_encoding
            .decode_word(&mut self.data)
            .map_err(|e| e.map_invalid("variant index"))? as u32)
    }
}

macro_rules! deserialize_int {
    ($name:ident, $visit:ident, $read:ident) => {
        fn $name<V>(self, visitor: V) -> Result<V::Value>
        where
            V: Visitor<'de>,
        {
            visitor.$visit(self.$read()?)
        }
    };
}

impl<'de, R: Read, N: NumericEncoding> Deserializer<'de> for &mut BitcodeDeserializer<R, N> {
    type Error = Error;

    fn deserialize_any<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        return Err(E::NotSupported("deserialize_any").e());
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_bool(self.read_bool()?)
    }

    deserialize_int!(deserialize_i8, visit_i8, read_i8);
    deserialize_int!(deserialize_i16, visit_i16, read_i16);
    deserialize_int!(deserialize_i32, visit_i32, read_i32);
    deserialize_int!(deserialize_i64, visit_i64, read_i64);
    deserialize_int!(deserialize_u8, visit_u8, read_u8);
    deserialize_int!(deserialize_u16, visit_u16, read_u16);
    deserialize_int!(deserialize_u32, visit_u32, read_u32);
    deserialize_int!(deserialize_u64, visit_u64, read_u64);

    fn deserialize_f32<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_f32(f32::from_bits(
            self.data.read_bits(u32::BITS as usize).map(|v| v as u32)?,
        ))
    }

    fn deserialize_f64<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_f64(f64::from_bits(self.data.read_bits(u64::BITS as usize)?))
    }

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let mut buf = [0; 4];
        buf[0] = self.read_u8()?;

        let len = utf8_char_width(buf[0]);
        if len > 1 {
            let bits = self.data.read_bits((len - 1) * u8::BITS as usize)?;
            buf[1..len].copy_from_slice(&bits.to_le_bytes()[0..len - 1]);
        }

        let s = std::str::from_utf8(&buf[..len]).map_err(|_| E::Invalid("char").e())?;
        debug_assert_eq!(s.as_bytes().len(), len);
        debug_assert_eq!(s.chars().count(), 1);
        visitor.visit_char(s.chars().next().unwrap())
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_string(visitor)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let bytes = self.read_len_and_bytes()?;
        visitor.visit_string(String::from_utf8(bytes).map_err(|_| E::Invalid("utf8").e())?)
    }

    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_byte_buf(visitor)
    }

    fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_byte_buf(self.read_len_and_bytes()?)
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        if self.read_bool()? {
            visitor.visit_some(self)
        } else {
            visitor.visit_none()
        }
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V>(self, _name: &'static str, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V>(self, _name: &'static str, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let len = self.read_len()?;
        self.deserialize_tuple(len, visitor)
    }

    // based on https://github.com/bincode-org/bincode/blob/c44b5e364e7084cdbabf9f94b63a3c7f32b8fb68/src/de/mod.rs#L293-L330
    fn deserialize_tuple<V>(self, len: usize, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        struct Access<'a, R, N> {
            deserializer: &'a mut BitcodeDeserializer<R, N>,
            len: usize,
        }

        impl<'de, R: Read, N: NumericEncoding> SeqAccess<'de> for Access<'_, R, N> {
            type Error = Error;

            fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>>
            where
                T: DeserializeSeed<'de>,
            {
                if self.len > 0 {
                    self.len -= 1;
                    let value = DeserializeSeed::deserialize(seed, &mut *self.deserializer)?;
                    Ok(Some(value))
                } else {
                    Ok(None)
                }
            }

            fn size_hint(&self) -> Option<usize> {
                Some(self.len)
            }
        }

        visitor.visit_seq(Access {
            deserializer: self,
            len,
        })
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_tuple(len, visitor)
    }

    // based on https://github.com/bincode-org/bincode/blob/c44b5e364e7084cdbabf9f94b63a3c7f32b8fb68/src/de/mod.rs#L353-L400
    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        struct Access<'a, R: Read, N> {
            deserializer: &'a mut BitcodeDeserializer<R, N>,
            len: usize,
        }

        impl<'de, R: Read, N: NumericEncoding> MapAccess<'de> for Access<'_, R, N> {
            type Error = Error;

            fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>>
            where
                K: DeserializeSeed<'de>,
            {
                if self.len > 0 {
                    self.len -= 1;
                    let key = DeserializeSeed::deserialize(seed, &mut *self.deserializer)?;
                    Ok(Some(key))
                } else {
                    Ok(None)
                }
            }

            fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value>
            where
                V: DeserializeSeed<'de>,
            {
                let value = DeserializeSeed::deserialize(seed, &mut *self.deserializer)?;
                Ok(value)
            }

            fn size_hint(&self) -> Option<usize> {
                Some(self.len)
            }
        }

        let len = self.read_len()?;
        visitor.visit_map(Access {
            deserializer: self,
            len,
        })
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_tuple(fields.len(), visitor)
    }

    // based on https://github.com/bincode-org/bincode/blob/c44b5e364e7084cdbabf9f94b63a3c7f32b8fb68/src/de/mod.rs#L263-L291
    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        impl<'a, 'de, R: Read, N: NumericEncoding> EnumAccess<'de> for &'a mut BitcodeDeserializer<R, N> {
            type Error = Error;
            type Variant = &'a mut BitcodeDeserializer<R, N>;

            fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant)>
            where
                V: DeserializeSeed<'de>,
            {
                let idx = self.read_variant_index()?;
                let val: Result<_> = seed.deserialize(idx.into_deserializer());
                Ok((val?, self))
            }
        }

        visitor.visit_enum(self)
    }

    fn deserialize_identifier<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        return Err(E::NotSupported("deserialize_identifier").e());
    }

    fn deserialize_ignored_any<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        return Err(E::NotSupported("deserialize_ignored_any").e());
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

// based on https://github.com/bincode-org/bincode/blob/c44b5e364e7084cdbabf9f94b63a3c7f32b8fb68/src/de/mod.rs#L461-L492
impl<'de, R: Read, N: NumericEncoding> VariantAccess<'de> for &mut BitcodeDeserializer<R, N> {
    type Error = Error;

    fn unit_variant(self) -> Result<()> {
        Ok(())
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value>
    where
        T: DeserializeSeed<'de>,
    {
        DeserializeSeed::deserialize(seed, self)
    }

    fn tuple_variant<V>(self, len: usize, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Deserializer::deserialize_tuple(self, len, visitor)
    }

    fn struct_variant<V>(self, fields: &'static [&'static str], visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Deserializer::deserialize_tuple(self, fields.len(), visitor)
    }
}
