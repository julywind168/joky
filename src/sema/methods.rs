//! Stable identities for trait implementations and resolved method calls.
use crate::module::StableId;

// Canonical identities produced by trait_method_name for the built-in protocols.
pub(crate) const ORDERING_NAME: &str = "@builtin/Ordering";
pub(crate) const PARTIAL_ORD_METHOD: &str = "@trait:5061727469616c4f7264:partial_compare";
pub(crate) const ORD_METHOD: &str = "@trait:4f7264:compare";
pub(crate) const PARTIAL_EQ_METHOD: &str = "@trait:5061727469616c4571:equals";
pub(crate) const SHOW_METHOD: &str = "@trait:53686f77:show";
pub(crate) const DEBUG_METHOD: &str = "@trait:4465627567:debug";
pub(crate) const DROP_METHOD: &str = "@trait:44726f70:drop";
pub(crate) const FROM_STRING_METHOD: &str = "@trait:46726f6d537472696e67:from_string";
pub(crate) const HASH_METHOD: &str = "@trait:48617368:hash";
pub(crate) const CURSOR_METHOD: &str = "@trait:437572736f72:advance";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ResolvedMethodCall {
    pub(crate) name: String,
    pub(crate) qualified: bool,
    pub(crate) static_target: Option<super::Type>,
}

pub(crate) fn trait_method_name(
    module: Option<StableId>,
    trait_name: &str,
    method: &str,
) -> String {
    let identity = module.map_or_else(
        || trait_name.into(),
        |module| super::import_methods::trait_key(module, trait_name),
    );
    format!("@trait:{}:{method}", hex::encode(identity.as_bytes()))
}

pub(super) fn method_source_name(name: &str) -> &str {
    if name.starts_with("@trait:") {
        name.rsplit_once(':').map_or(name, |(_, method)| method)
    } else {
        name
    }
}

pub(super) fn method_trait_name(name: &str) -> Option<String> {
    let (identity, _) = name.strip_prefix("@trait:")?.rsplit_once(':')?;
    String::from_utf8(hex::decode(identity).ok()?).ok()
}

/// Built-in supertraits; custom trait inheritance is not implied here.
pub(crate) fn builtin_bounds(name: &str) -> Vec<&str> {
    match name {
        "Ord" => vec!["Ord", "PartialOrd", "Eq", "PartialEq"],
        "PartialOrd" => vec!["PartialOrd", "PartialEq"],
        "Eq" => vec!["Eq", "PartialEq"],
        _ => vec![name],
    }
}

impl super::checker::Checker {
    pub(super) fn associated_type_value(
        &self,
        receiver: super::Type,
        trait_name: &str,
        name: &str,
    ) -> Option<super::Type> {
        if let super::Type::List(id) = receiver {
            if trait_name == "Cursor" && name == "Item" {
                return Some(self.list_types[id]);
            }
        }
        if trait_name == "Cursor" && name == "Item" {
            match receiver {
                super::Type::BytesCursor => return Some(super::Type::U8),
                // The (K, V) entry tuple is interned when the cursor type is
                // created; look it up instead of interning under &self.
                super::Type::MapCursor(id) => {
                    let info = self.maps[id];
                    let elements = [info.key, info.value];
                    return self
                        .tuple_types
                        .iter()
                        .position(|candidate| *candidate == elements)
                        .map(super::Type::Tuple);
                }
                super::Type::MapKeyCursor(id) => return Some(self.maps[id].key),
                super::Type::MapValueCursor(id) => return Some(self.maps[id].value),
                super::Type::MutListCursor(id) => return Some(self.list_types[id]),
                super::Type::MutMapCursor(id) => {
                    let info = self.maps[id];
                    let elements = [info.key, info.value];
                    return self
                        .tuple_types
                        .iter()
                        .position(|candidate| *candidate == elements)
                        .map(super::Type::Tuple);
                }
                super::Type::MutSetCursor(id) => return Some(self.maps[id].key),
                _ => {}
            }
        }
        self.trait_associated_impls
            .get(&(trait_name.into(), receiver))
            .and_then(|values| values.get(name))
            .copied()
    }
}
