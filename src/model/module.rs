// AluVM Assembler
// To find more on AluVM please check <https://www.aluvm.org>
//
// Designed & written in 2021 by
//     Dr. Maxim Orlovsky <orlovsky@pandoracore.com>
// for Pandora Core AG

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::string::FromUtf8Error;
use std::vec::IntoIter;

use aluvm::data::encoding::{Decode, DecodeError, Encode, EncodeError, MaxLenByte, MaxLenWord};
use aluvm::data::{ByteStr, FloatLayout, IntLayout, Layout, MaybeNumber, Number, NumberLayout};
use aluvm::libs::constants::{ISAE_SEGMENT_MAX_LEN, LIBS_SEGMENT_MAX_COUNT};
use aluvm::libs::{LibId, LibSeg, LibSegOverflow, LibSite};
use amplify::IoError;

#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display, Error, From)]
#[display(doc_comments)]
pub enum CallTableError {
    /// library `{0}` is not found
    LibNotFound(LibId),

    /// call table for library `{0}` is not found
    LibTableNotFound(LibId),

    /// routine reference #`{1}` entry in library `{0}` call table is not found
    RoutineNotFound(LibId, u16),

    /// number of external routine calls exceeds maximal number of jumps allowed by VM's `cy0`
    TooManyRoutines,

    /// number of external libraries exceeds maximum
    TooManyLibs,
}

#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Default)]
pub struct CallRef {
    pub routine: String,
    pub sites: BTreeSet<u16>,
}

#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Default)]
pub struct CallTable(BTreeMap<LibId, Vec<CallRef>>);

impl CallTable {
    pub fn get_mut(&mut self, site: LibSite) -> Result<&mut CallRef, CallTableError> {
        self.0
            .get_mut(&site.lib)
            .ok_or(CallTableError::LibTableNotFound(site.lib))?
            .get_mut(site.pos as usize)
            .ok_or(CallTableError::RoutineNotFound(site.lib, site.pos))
    }

    pub fn find_or_insert(&mut self, id: LibId, routine: &str) -> Result<u16, CallTableError> {
        if self.0.len() >= u16::MAX as usize {
            return Err(CallTableError::TooManyRoutines);
        }
        if self.0.len() >= LIBS_SEGMENT_MAX_COUNT {
            return Err(CallTableError::TooManyLibs);
        }
        let vec = self.0.entry(id).or_default();
        let pos =
            vec.iter_mut().position(|callref| callref.routine == routine).unwrap_or_else(|| {
                let callref = CallRef { routine: routine.to_owned(), sites: bset![] };
                vec.push(callref);
                vec.len() - 1
            });
        Ok(pos as u16)
    }

    pub fn routines(&self) -> IntoIter<&str> {
        self.0
            .iter()
            .flat_map(|(id, routines)| {
                routines.into_iter().map(|call_ref| call_ref.routine.as_str())
            })
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub fn call_refs(&self) -> IntoIter<(LibId, &str, &BTreeSet<u16>)> {
        self.0
            .iter()
            .flat_map(|(id, routines)| {
                routines
                    .into_iter()
                    .map(move |call_ref| (*id, call_ref.routine.as_str(), &call_ref.sites))
            })
            .collect::<Vec<_>>()
            .into_iter()
    }
}

#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug)]
pub enum DataType {
    ByteStr(Option<Vec<u8>>),
    Int(IntLayout, MaybeNumber),
    Float(FloatLayout, MaybeNumber),
}

#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug)]
pub struct Variable {
    pub info: String,
    pub data: DataType,
}

#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Default)]
pub struct Module {
    pub isae: String,
    pub code: Vec<u8>,
    pub data: Vec<u8>,
    pub libs: LibSeg,
    pub vars: Vec<Variable>,
    pub imports: CallTable,
    /// Map of local routine names to code offsets
    pub exports: BTreeMap<String, u16>,
}

/// TODO: use in decoding (currently unused, had left after refactoring)
#[derive(Clone, Eq, PartialEq, Debug, Display, Error, From)]
#[display(doc_comments)]
pub enum ModuleError {
    /// end of data is reached before the complete module read
    /// \n
    /// details: {0}
    #[from]
    #[from(io::Error)]
    Io(IoError),

    /// length of ISA extensions segment is {0} exceeds limit
    IsaeLengthLimExceeded(usize),

    /// module contains too many libraries exceeding per-module lib limit
    #[from(LibSegOverflow)]
    LibCountLimExceeded,

    /// input variable description has a non-UTF8 encoding
    /// \n
    /// details: {0}
    VarNonUtf8(FromUtf8Error),

    /// routine symbol name has a non-UTF8 encoding
    /// \n
    /// details: {0}
    RoutineNonUtf8(FromUtf8Error),

    /// external call symbol has a non-UTF8 encoding
    /// \n
    /// details: {0}
    ExternalNonUtf8(FromUtf8Error),

    /// unknown type byte `{0}` for input variable having description "{1}"
    VarUnknownType(u8, String),

    /// wrong sign integer layout byte `{0}` for input variable having description "{1}"
    VarWrongSignByte(u8, String),

    /// layout size ({layout_bytes} bytes) does not match {data_bytes} size of the default value
    /// for variable with description "{info}"
    VarWrongLayout { layout_bytes: u16, data_bytes: u16, info: String },

    /// unknown float layout type `{0}` for input variable having description "{1}"
    VarWrongFloatType(u8, String),
}

impl Encode for DataType {
    type Error = EncodeError;

    fn encode(&self, mut writer: impl Write) -> Result<usize, Self::Error> {
        match self {
            DataType::ByteStr(bytestr) => {
                return Ok(0xFF_u8.encode(&mut writer)?
                    + bytestr.as_ref().map(ByteStr::with).encode(&mut writer)?)
            }
            DataType::Int(layout, default) => (Layout::from(*layout), default),
            DataType::Float(layout, default) => (Layout::from(*layout), default),
        }
        .encode(&mut writer)
    }
}

impl Decode for DataType {
    type Error = DecodeError;

    fn decode(mut reader: impl Read) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Ok(match u8::decode(&mut reader)? {
            0xFF => {
                let data: Vec<u8> = MaxLenWord::decode(&mut reader)?.release();
                let inner = if data.is_empty() { None } else { Some(data) };
                DataType::ByteStr(inner)
            }

            i if i <= 1 => DataType::Int(
                IntLayout { signed: i == 1, bytes: u16::decode(&mut reader)? },
                MaybeNumber::decode(&mut reader)?,
            ),

            f => DataType::Float(
                FloatLayout::with(f).ok_or(DecodeError::FloatLayout(f))?,
                MaybeNumber::decode(&mut reader)?,
            ),
        })
    }
}

impl Encode for Variable {
    type Error = EncodeError;

    fn encode(&self, mut writer: impl Write) -> Result<usize, Self::Error> {
        Ok(self.info.encode(&mut writer)? + self.data.encode(&mut writer)?)
    }
}

impl Decode for Variable {
    type Error = DecodeError;

    fn decode(mut reader: impl Read) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Ok(Variable { info: Decode::decode(&mut reader)?, data: Decode::decode(&mut reader)? })
    }
}

impl Encode for CallRef {
    type Error = EncodeError;

    fn encode(&self, mut writer: impl Write) -> Result<usize, Self::Error> {
        Ok(self.routine.encode(&mut writer)? + MaxLenWord::new(&self.sites).encode(&mut writer)?)
    }
}

impl Decode for CallRef {
    type Error = DecodeError;

    fn decode(mut reader: impl Read) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Ok(CallRef {
            routine: Decode::decode(&mut reader)?,
            sites: MaxLenWord::decode(&mut reader)?.release(),
        })
    }
}

impl Encode for CallTable {
    type Error = EncodeError;

    fn encode(&self, mut writer: impl Write) -> Result<usize, Self::Error> {
        let len = self.0.len() as u8;
        let mut count = len.encode(&mut writer)?;
        for (lib, map) in &self.0 {
            count += lib.encode(&mut writer)?;
            count += MaxLenWord::new(map).encode(&mut writer)?;
        }
        Ok(count)
    }
}

impl Decode for CallTable {
    type Error = DecodeError;

    fn decode(mut reader: impl Read) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        let len = u8::decode(&mut reader)?;
        let mut table = bmap! {};
        for _ in 0..len {
            table.insert(LibId::decode(&mut reader)?, MaxLenWord::decode(&mut reader)?.release());
        }
        Ok(CallTable(table))
    }
}

impl Encode for Module {
    type Error = EncodeError;

    fn encode(&self, mut writer: impl Write) -> Result<usize, Self::Error> {
        Ok(self.isae.encode(&mut writer)?
            + ByteStr::with(&self.code).encode(&mut writer)?
            + ByteStr::with(&self.data).encode(&mut writer)?
            + self.libs.encode(&mut writer)?
            + self.imports.encode(&mut writer)?
            + MaxLenWord::new(&self.exports).encode(&mut writer)?
            + MaxLenWord::new(&self.vars).encode(&mut writer)?)
    }
}

impl Decode for Module {
    type Error = DecodeError;

    fn decode(mut reader: impl Read) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Ok(Module {
            isae: Decode::decode(&mut reader)?,
            code: ByteStr::decode(&mut reader)?.to_vec(),
            data: ByteStr::decode(&mut reader)?.to_vec(),
            libs: Decode::decode(&mut reader)?,
            imports: Decode::decode(&mut reader)?,
            exports: MaxLenWord::decode(&mut reader)?.release(),
            vars: MaxLenWord::decode(&mut reader)?.release(),
        })
    }
}
