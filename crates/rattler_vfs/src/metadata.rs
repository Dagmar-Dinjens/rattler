use std::{ffi::{OsStr, OsString}, path::{Path, PathBuf}, sync::Arc};



#[derive(Debug)]
pub struct fsDirectory {
    pub prefix_path: PathBuf,
    pub parent: usize,
    pub children: Vec<usize>,
}

impl fsDirectory{
    fn new(prefix_path: PathBuf, parent: usize) -> Self {
        fsDirectory{
            prefix_path,
            parent,
            children: vec![],
        }
    }
}

#[derive(Debug)]
pub struct fsFile{
    pub file_name: OsString,
    pub parent: usize,
    pub cache_base_path: Arc<Path>,
    pub _path_type: PathType,
    pub prefix_placeholder: Option<PrefixPlaceholder>,
}

impl fsFile {
    fn new(
        file_name: OsString,
        parent: usize,
        cache_base_path: Arc<Path>,
        _path_type: PathType, 
        prefix_placeholder: Option<PrefixPlaceholder>,
    ) -> Self {
        File{
            file_name,
            parent,
            cache_base_path,
            _path_type,
            prefix_placeholder,
        }
    }
}

#[derive(Debug)]
pub enum Metadata{
    Directory(fsDirectory),
    File(fsFile)
}

impl Metadata {
    pub fn file_name(&self) -> &OsStr {
        match self {
            Self::Directory(directory) => directory.prefix_path.file_name().unwrap(),
            Self::File(file) => &file.file_name
        }
    }
    pub fn new_directory (prefix_path: PathBuf, parent: usize) -> Self {
        Metadata::fsDirectory(fsDirectory::new(prefix_path, parent))
    }

    pub fn new_file(
        file_name: OsString,
        parent: usize,
        cache_base_path: Arc<Path>,
        path_type: PathType, 
        prefix_placeholder:
        Option<PrefixPlaceholder>
    ) -> Self {
        Metadata::File(File::new(file_name, parent, cache_base_path, path_type, prefix_placeholder))
    }

    pub fn as_directory(&self) -> Option<&Directory> {
        match self {
            Self::Directory(directory) => Some(directory),
            Self::File(_) => None
        }
    }
    pub fn as_directory_mut(&mut self) -> Option<&mut Directory> {
        match self {
            Self::Directory(directory) => Some(directory),
            Self::File(_) => None
        }
    }
    pub fn as_file(&self) -> Option<&File>{
        match self {
            Self::File(file) => Some(file),
            Self::Directory(_) => None
        }
    }
    
    pub fn as_file_mut(&mut self)  -> Option<&mut File>{
        match self {
            Self::File(file) => Some(file),
            Self::Directory(_) => None
        }
    }
}