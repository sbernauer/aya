use std::{
    cell::{Ref, RefCell, RefMut},
    collections::HashMap,
    convert::TryFrom,
    io,
};

use thiserror::Error;

use crate::{
    maps::{Map, MapError},
    obj::btf::RelocationError as BtfRelocationError,
    obj::{btf::BtfError, Object, ParseError, RelocationError},
    programs::{KProbe, Program, ProgramData, ProgramError, SocketFilter, TracePoint, UProbe, Xdp},
    syscalls::bpf_map_update_elem_ptr,
};

pub(crate) const BPF_OBJ_NAME_LEN: usize = 16;

/* FIXME: these are arch dependent */
pub(crate) const PERF_EVENT_IOC_ENABLE: libc::c_ulong = 9216;
pub(crate) const PERF_EVENT_IOC_DISABLE: libc::c_ulong = 9217;
pub(crate) const PERF_EVENT_IOC_SET_BPF: libc::c_ulong = 1074013192;

pub unsafe trait Pod: Copy + 'static {}

macro_rules! unsafe_impl_pod {
    ($($struct_name:ident),+ $(,)?) => {
        $(
            unsafe impl Pod for $struct_name { }
        )+
    }
}

unsafe_impl_pod!(i8, u8, i16, u16, i32, u32, i64, u64);

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub(crate) struct bpf_map_def {
    pub(crate) map_type: u32,
    pub(crate) key_size: u32,
    pub(crate) value_size: u32,
    pub(crate) max_entries: u32,
    pub(crate) map_flags: u32,
}

#[derive(Debug)]
pub struct Bpf {
    maps: HashMap<String, RefCell<Map>>,
    programs: HashMap<String, Program>,
}

impl Bpf {
    pub fn load(data: &[u8]) -> Result<Bpf, BpfError> {
        let mut obj = Object::parse(data)?;

        obj.relocate_btf()?;

        let mut maps = Vec::new();
        for (_, obj) in obj.maps.drain() {
            let mut map = Map { obj, fd: None };
            let fd = map.create()?;
            if !map.obj.data.is_empty() && map.obj.name != ".bss" {
                bpf_map_update_elem_ptr(fd, &0 as *const _, map.obj.data.as_ptr(), 0)
                    .map_err(|(code, io_error)| MapError::UpdateElementFailed { code, io_error })?;
            }
            maps.push(map);
        }

        obj.relocate_maps(maps.as_slice())?;

        let programs = obj
            .programs
            .drain()
            .map(|(name, obj)| {
                let kind = obj.kind;
                let data = ProgramData {
                    obj,
                    name: name.clone(),
                    fd: None,
                    links: Vec::new(),
                };
                let program = match kind {
                    crate::obj::ProgramKind::KProbe => Program::KProbe(KProbe { data }),
                    crate::obj::ProgramKind::UProbe => Program::UProbe(UProbe { data }),
                    crate::obj::ProgramKind::TracePoint => Program::TracePoint(TracePoint { data }),
                    crate::obj::ProgramKind::SocketFilter => {
                        Program::SocketFilter(SocketFilter { data })
                    }
                    crate::obj::ProgramKind::Xdp => Program::Xdp(Xdp { data }),
                };

                (name, program)
            })
            .collect();

        Ok(Bpf {
            maps: maps
                .drain(..)
                .map(|map| (map.obj.name.clone(), RefCell::new(map)))
                .collect(),
            programs,
        })
    }

    pub fn map<'a, 'slf: 'a, T: TryFrom<Ref<'a, Map>>>(
        &'slf self,
        name: &str,
    ) -> Result<Option<T>, <T as TryFrom<Ref<'a, Map>>>::Error> {
        self.maps
            .get(name)
            .map(|cell| T::try_from(cell.borrow()))
            .transpose()
    }

    pub fn map_mut<'a, 'slf: 'a, T: TryFrom<RefMut<'a, Map>>>(
        &'slf self,
        name: &str,
    ) -> Result<Option<T>, <T as TryFrom<RefMut<'a, Map>>>::Error> {
        self.maps
            .get(name)
            .map(|cell| T::try_from(cell.borrow_mut()))
            .transpose()
    }

    pub fn program<'a, 'slf: 'a, T: TryFrom<&'a Program>>(
        &'slf self,
        name: &str,
    ) -> Result<Option<T>, <T as TryFrom<&'a Program>>::Error> {
        self.programs.get(name).map(|p| T::try_from(p)).transpose()
    }

    pub fn program_mut<'a, 'slf: 'a, T: TryFrom<&'a mut Program>>(
        &'slf mut self,
        name: &str,
    ) -> Result<Option<T>, <T as TryFrom<&'a mut Program>>::Error> {
        self.programs
            .get_mut(name)
            .map(|p| T::try_from(p))
            .transpose()
    }
}

#[derive(Debug, Error)]
pub enum BpfError {
    #[error("IO error: {0}")]
    IO(#[from] io::Error),

    #[error("error parsing BPF object: {0}")]
    ParseError(#[from] ParseError),

    #[error("BTF error: {0}")]
    BtfError(#[from] BtfError),

    #[error("error relocating BPF object: {0}")]
    RelocationError(#[from] RelocationError),

    #[error(transparent)]
    BtfRelocationError(#[from] BtfRelocationError),

    #[error("map error: {0}")]
    MapError(#[from] MapError),

    #[error("program error: {0}")]
    ProgramError(#[from] ProgramError),
}
