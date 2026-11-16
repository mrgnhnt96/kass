mod api;
mod ftl;

use fluent_bundle::FluentResource;
use fluent_syntax::ast::Entry;

/// Ids of every message in one `.ftl` source, in file order.
pub(super) fn message_ids(source: &str) -> Vec<String> {
    let resource = match FluentResource::try_new(source.to_owned()) {
        Ok(resource) => resource,
        Err((resource, _)) => resource,
    };
    resource
        .entries()
        .filter_map(|entry| match entry {
            Entry::Message(message) => Some(message.id.name.to_owned()),
            _ => None,
        })
        .collect()
}
