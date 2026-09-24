//! Private compiler hooks backing the public, pure joky/path functions.
use super::{Type, TypeTable};

pub(crate) fn operation(name: &str) -> Option<u8> {
    Some(match name {
        "__joky_path_join" => 0,
        "__joky_path_is_absolute" => 1,
        "__joky_path_parent" => 2,
        "__joky_path_file_name" => 3,
        "__joky_path_file_stem" => 4,
        "__joky_path_extension" => 5,
        "__joky_path_with_extension" => 6,
        "__joky_path_strip_prefix" => 7,
        "__joky_path_split_paths" => 8,
        "__joky_path_join_paths" => 9,
        _ => return None,
    })
}

pub(crate) fn signature(op: u8, types: &TypeTable) -> Option<(Vec<Type>, Type)> {
    let inputs = match op {
        0 | 6 | 7 => vec![Type::String, Type::String],
        9 => vec![Type::List(types.list_id(Type::String)?)],
        1..=5 | 8 => vec![Type::String],
        _ => return None,
    };
    let result = match op {
        0 | 6 => Type::String,
        1 => Type::Bool,
        2..=5 | 7 => Type::Option(types.option_id(Type::String)?),
        8 => Type::List(types.list_id(Type::String)?),
        9 => Type::Result(types.result_id(Type::String, Type::String)?),
        _ => return None,
    };
    Some((inputs, result))
}
