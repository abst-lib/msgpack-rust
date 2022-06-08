//! Provides various functions and structs for MessagePack decoding.
//!
//! Most of the function defined in this module will silently handle interruption error (EINTR)
//! received from the given `Read` to be in consistent state with the `Write::write_all` method in
//! the standard library.
//!
//! Any other error would immediately interrupt the parsing process. If your reader can results in
//! I/O error and simultaneously be a recoverable state (for example, when reading from
//! non-blocking socket and it returns EWOULDBLOCK) be sure that you buffer the data externally
//! to avoid data loss (using `BufRead` readers with manual consuming or some other way).

pub(crate) mod dec;
pub(crate) mod ext;
pub(crate) mod sint;
pub(crate) mod str;
pub(crate) mod uint;


pub use crate::sync::decode::dec::{read_f32, read_f64};
pub use crate::sync::decode::ext::{
    read_ext_meta, read_fixext1, read_fixext16, read_fixext2, read_fixext4, read_fixext8,
};
pub use crate::sync::decode::sint::{read_i16, read_i32, read_i64, read_i8, read_nfix};
// While we re-export deprecated items, we don't want to trigger warnings while compiling this crate
pub use crate::sync::decode::str::{read_str, read_str_from_slice, read_str_len, read_str_ref};
pub use crate::sync::decode::uint::{read_pfix, read_u16, read_u32, read_u64, read_u8};

use num_traits::cast::FromPrimitive;

use crate::Marker;

pub mod bytes;
pub use crate::decode::Bytes;
pub use crate::decode::{NumValueReadError, ValueReadError, RmpReadErr};
use crate::decode::MarkerReadError;

#[doc(inline)]
#[allow(deprecated)]
pub use crate::errors::Error;




macro_rules! read_byteorder_utils {
    ($($name:ident => $tp:ident),* $(,)?) => {
        $(
            #[inline]
            #[doc(hidden)]
            fn $name(&mut self) -> Result<$tp, ValueReadError<Self::Error>> where Self: Sized {
                const SIZE: usize = core::mem::size_of::<$tp>();
                let mut buf: [u8; SIZE] = [0u8; SIZE];
                self.read_exact_buf(&mut buf).map_err(ValueReadError::InvalidDataRead)?;
                Ok(paste::paste! {
                    <byteorder::BigEndian as byteorder::ByteOrder>::[<read_ $tp>](&mut buf)
                })
            }
        )*
    };
}
mod sealed {
    pub trait Sealed {}

    #[cfg(feature = "std")]
    impl<T: ?Sized + std::io::Read> Sealed for T {}

    #[cfg(not(feature = "std"))]
    impl<'a> Sealed for &'a [u8] {}

    impl Sealed for super::Bytes<'_> {}
}


/// A type that `rmp` supports reading from.
///
/// The methods of this trait should be considered an implementation detail (for now).
/// It is currently sealed (can not be implemented by the user).
///
/// See also [std::io::Read] and [byteorder::ReadBytesExt]
///
/// Its primary implementations are [std::io::Read] and [Bytes].
pub trait RmpRead: sealed::Sealed {
    type Error: RmpReadErr;
    /// Read a single (unsigned) byte from this stream
    #[inline]
    fn read_u8(&mut self) -> Result<u8, Self::Error> {
        let mut buf = [0; 1];
        self.read_exact_buf(&mut buf)?;
        Ok(buf[0])
    }

    /// Read the exact number of bytes needed to fill the specified buffer.
    ///
    /// If there are not enough bytes, this will return an error.
    ///
    /// See also [std::io::Read::read_exact]
    fn read_exact_buf(&mut self, buf: &mut [u8]) -> Result<(), Self::Error>;

    // Internal helper functions to map I/O error into the `InvalidDataRead` error.

    /// Read a single (unsigned) byte from this stream.
    #[inline]
    #[doc(hidden)]
    fn read_data_u8(&mut self) -> Result<u8, ValueReadError<Self::Error>> {
        self.read_u8().map_err(ValueReadError::InvalidDataRead)
    }
    /// Read a single (signed) byte from this stream.
    #[inline]
    #[doc(hidden)]
    fn read_data_i8(&mut self) -> Result<i8, ValueReadError<Self::Error>> {
        self.read_data_u8().map(|b| b as i8)
    }

    read_byteorder_utils!(
        read_data_u16 => u16,
        read_data_u32 => u32,
        read_data_u64 => u64,
        read_data_i16 => i16,
        read_data_i32 => i32,
        read_data_i64 => i64,
        read_data_f32 => f32,
        read_data_f64 => f64
    );
}

/*
 * HACK: rmpv & rmp-erde used the internal read_data_* functions.
 *
 * Since adding no_std support moved these functions to the RmpRead trait,
 * this broke compatiblity  (despite changing no public APIs).
 *
 * In theory, we could update rmpv and rmp-serde to use the new APIS,
 * but that would be needless churn (and might surprise users who just want to update rmp proper).
 *
 * Instead, we emulate these internal APIs for now,
 * so that rmpv and rmp-serde continue to compile without issue.
 *
 *
 * TODO: Remove this hack once we release a new version of rmp proper
 */

macro_rules! wrap_data_funcs_for_compatibility {
    ($($tp:ident),* $(,)?) => {
        $(paste::paste! {
            #[cfg(feature = "std")]
            #[doc(hidden)]
            #[deprecated(note = "internal function. rmpv & rmp-serde need to switch to RmpRead")]
            pub fn [<read_data_ $tp>] <R: std::io::Read>(buf: &mut R) -> Result<$tp, ValueReadError> {
                buf.[<read_data_ $tp>]()
            }
        })*
    };
}
wrap_data_funcs_for_compatibility!(
    u8, u16, u32, u64,
    i8, i16, i32, i64,
    f32, f64
);

#[cfg(feature = "std")]
impl<T: std::io::Read> RmpRead for T {
    type Error = std::io::Error;

    #[inline]
    fn read_exact_buf(&mut self, buf: &mut [u8]) -> Result<(), Self::Error> {
        std::io::Read::read_exact(self, buf)
    }
}

/// Attempts to read a single byte from the given reader and to decode it as a MessagePack marker.
#[inline]
pub fn read_marker<R: RmpRead>(rd: &mut R) -> Result<Marker, MarkerReadError<R::Error>> {
    Ok(Marker::from_u8(rd.read_u8()?))
}

/// Attempts to read a single byte from the given reader and to decode it as a nil value.
///
/// According to the MessagePack specification, a nil value is represented as a single `0xc0` byte.
///
/// # Errors
///
/// This function will return `ValueReadError` on any I/O error while reading the nil marker,
/// except the EINTR, which is handled internally.
///
/// It also returns `ValueReadError::TypeMismatch` if the actual type is not equal with the
/// expected one, indicating you with the actual type.
///
/// # Note
///
/// This function will silently retry on every EINTR received from the underlying `Read` until
/// successful read.
pub fn read_nil<R: RmpRead>(rd: &mut R) -> Result<(), ValueReadError<R::Error>> {
    match read_marker(rd)? {
        Marker::Null => Ok(()),
        marker => Err(ValueReadError::TypeMismatch(marker)),
    }
}

/// Attempts to read a single byte from the given reader and to decode it as a boolean value.
///
/// According to the MessagePack specification, an encoded boolean value is represented as a single
/// byte.
///
/// # Errors
///
/// This function will return `ValueReadError` on any I/O error while reading the bool marker,
/// except the EINTR, which is handled internally.
///
/// It also returns `ValueReadError::TypeMismatch` if the actual type is not equal with the
/// expected one, indicating you with the actual type.
///
/// # Note
///
/// This function will silently retry on every EINTR received from the underlying `Read` until
/// successful read.
pub fn read_bool<R: RmpRead>(rd: &mut R) -> Result<bool, ValueReadError<R::Error>> {
    match read_marker(rd)? {
        Marker::True => Ok(true),
        Marker::False => Ok(false),
        marker => Err(ValueReadError::TypeMismatch(marker)),
    }
}


/// Attempts to read up to 9 bytes from the given reader and to decode them as integral `T` value.
///
/// This function will try to read up to 9 bytes from the reader (1 for marker and up to 8 for data)
/// and interpret them as a big-endian `T`.
///
/// Unlike `read_*`, this function weakens type restrictions, allowing you to safely decode packed
/// values even if you aren't sure about the actual integral type.
///
/// # Errors
///
/// This function will return `NumValueReadError` on any I/O error while reading either the marker
/// or the data.
///
/// It also returns `NumValueReadError::OutOfRange` if the actual type is not an integer or it does
/// not fit in the given numeric range.
///
/// # Examples
///
/// ```
/// let buf = [0xcd, 0x1, 0x2c];
///
/// assert_eq!(300u16, rmp::decode::read_int(&mut &buf[..]).unwrap());
/// assert_eq!(300i16, rmp::decode::read_int(&mut &buf[..]).unwrap());
/// assert_eq!(300u32, rmp::decode::read_int(&mut &buf[..]).unwrap());
/// assert_eq!(300i32, rmp::decode::read_int(&mut &buf[..]).unwrap());
/// assert_eq!(300u64, rmp::decode::read_int(&mut &buf[..]).unwrap());
/// assert_eq!(300i64, rmp::decode::read_int(&mut &buf[..]).unwrap());
/// assert_eq!(300usize, rmp::decode::read_int(&mut &buf[..]).unwrap());
/// assert_eq!(300isize, rmp::decode::read_int(&mut &buf[..]).unwrap());
/// ```
pub fn read_int<T: FromPrimitive, R: RmpRead>(rd: &mut R) -> Result<T, NumValueReadError<R::Error>> {
    let val = match read_marker(rd)? {
        Marker::FixPos(val) => T::from_u8(val),
        Marker::FixNeg(val) => T::from_i8(val),
        Marker::U8 => T::from_u8(rd.read_data_u8()?),
        Marker::U16 => T::from_u16(rd.read_data_u16()?),
        Marker::U32 => T::from_u32(rd.read_data_u32()?),
        Marker::U64 => T::from_u64(rd.read_data_u64()?),
        Marker::I8 => T::from_i8(rd.read_data_i8()?),
        Marker::I16 => T::from_i16(rd.read_data_i16()?),
        Marker::I32 => T::from_i32(rd.read_data_i32()?),
        Marker::I64 => T::from_i64(rd.read_data_i64()?),
        marker => return Err(NumValueReadError::TypeMismatch(marker)),
    };

    val.ok_or(NumValueReadError::OutOfRange)
}

/// Attempts to read up to 5 bytes from the given reader and to decode them as a big-endian u32
/// array size.
///
/// Array format family stores a sequence of elements in 1, 3, or 5 bytes of extra bytes in addition
/// to the elements.
///
/// # Note
///
/// This function will silently retry on every EINTR received from the underlying `Read` until
/// successful read.
// TODO: Docs.
// NOTE: EINTR is managed internally.
pub fn read_array_len<R>(rd: &mut R) -> Result<u32, ValueReadError<R::Error>>
    where
        R: RmpRead,
{
    match read_marker(rd)? {
        Marker::FixArray(size) => Ok(size as u32),
        Marker::Array16 => Ok(rd.read_data_u16()? as u32),
        Marker::Array32 => Ok(rd.read_data_u32()?),
        marker => Err(ValueReadError::TypeMismatch(marker)),
    }
}

/// Attempts to read up to 5 bytes from the given reader and to decode them as a big-endian u32
/// map size.
///
/// Map format family stores a sequence of elements in 1, 3, or 5 bytes of extra bytes in addition
/// to the elements.
///
/// # Note
///
/// This function will silently retry on every EINTR received from the underlying `Read` until
/// successful read.
// TODO: Docs.
pub fn read_map_len<R: RmpRead>(rd: &mut R) -> Result<u32, ValueReadError<R::Error>> {
    let marker = read_marker(rd)?;
    marker_to_len(rd, marker)
}

pub fn marker_to_len<R: RmpRead>(rd: &mut R, marker: Marker) -> Result<u32, ValueReadError<R::Error>> {
    match marker {
        Marker::FixMap(size) => Ok(size as u32),
        Marker::Map16 => Ok(rd.read_data_u16()? as u32),
        Marker::Map32 => Ok(rd.read_data_u32()?),
        marker => Err(ValueReadError::TypeMismatch(marker)),
    }
}

/// Attempts to read up to 5 bytes from the given reader and to decode them as Binary array length.
///
/// # Note
///
/// This function will silently retry on every EINTR received from the underlying `Read` until
/// successful read.
// TODO: Docs.
pub fn read_bin_len<R: RmpRead>(rd: &mut R) -> Result<u32, ValueReadError<R::Error>> {
    match read_marker(rd)? {
        Marker::Bin8 => Ok(rd.read_data_u8()? as u32),
        Marker::Bin16 => Ok(rd.read_data_u16()? as u32),
        Marker::Bin32 => Ok(rd.read_data_u32()?),
        marker => Err(ValueReadError::TypeMismatch(marker)),
    }
}