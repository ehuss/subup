use cargo_metadata::{Metadata, Node};
use std::collections::{HashMap, HashSet};

type ResolveMap<'a> = HashMap<&'a str, &'a Node>;

/// Compare two Metdata instances, and find any differences.
///
/// `modified_members` is a set of member names that were possibly updated,
/// even if their manifest did not change.
///
/// Returns `(dep_trails, root_paths`).
///
/// `dep_trails` is a list of dependency trails for the dependencies that
/// differ. Each element in the list is an "id" with the format
/// "NAME VERSION".
///
/// `root_paths` is the set of paths to workspace members that are affected.
pub fn diff_resolve(
    first: &Metadata,
    second: &Metadata,
    modified_members: &HashSet<String>,
) -> (Vec<Vec<String>>, HashSet<String>) {
    let mut result = Vec::new();
    // Get the `id` of all of the workspace members.
    // TODO: This probably shouldn't ignore source.
    let roots: HashSet<(String, String)> = first
        .workspace_members
        .iter()
        .map(|wm| (wm.name.clone(), format!("{}", wm.version)))
        .collect();
    // TODO: Verify all roots were found.
    let root_ids: HashSet<String> = first
        .packages
        .iter()
        .filter_map(|p| {
            if roots.contains(&(p.name.clone(), p.version.clone())) {
                Some(p.id.clone())
            } else {
                None
            }
        })
        .collect();
    // Create `id`s of the modified members.
    // These will be treated as-if they are changed in the manifest.
    let modified_member_ids = modified_members
        .iter()
        .map(|name| {
            let member = first
                .workspace_members
                .iter()
                .find(|m| m.name == *name)
                .unwrap_or_else(|| panic!("Could not find `{}` in workspace members.", name));
            format!("{} {}", member.name, member.version)
        })
        .collect();
    // Create maps of the resolves for easy access.
    let first_resolve: ResolveMap = first
        .resolve
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();
    let second_resolve: ResolveMap = second
        .resolve
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();

    // Diff each root independently.
    for id in &root_ids {
        let mut trail = Vec::new();
        diff(
            &modified_member_ids,
            &first_resolve,
            &second_resolve,
            id,
            &mut trail,
            &mut result,
        );
    }

    // Based on the diffs, get the path to each workspace member that is
    // affected.
    let mut changed_paths = HashSet::new();
    for trail in &result {
        let name = trail[0].split(' ').next().unwrap();
        // TODO: Use find() instead?
        let mut found = false;
        for member in &first.workspace_members {
            if member.name == name {
                // TODO: When is this not the case?
                assert!(member.url.starts_with("path+file:/"));
                changed_paths.insert(member.url[11..].to_string());
                found = true;
                break;
            }
        }
        if !found {
            panic!("Did not find root `{}` in workspace.", name);
        }
    }

    (result, changed_paths)
}

fn strip_source(id: &str) -> String {
    let parts: Vec<_> = id.splitn(3, ' ').collect();
    format!("{} {}", parts[0], parts[1])
}

fn diff(
    modified_members: &HashSet<String>,
    first_resolve: &ResolveMap,
    second_resolve: &ResolveMap,
    id: &str,
    trail: &mut Vec<String>,
    result: &mut Vec<Vec<String>>,
) {
    // This could be better.
    trail.push(strip_source(id));
    if !second_resolve.contains_key(id) || modified_members.contains(&strip_source(id)) {
        result.push(trail.clone());
        trail.pop();
        return;
    }
    let first_deps = &first_resolve[id].dependencies;
    let second_deps = &second_resolve[id].dependencies;
    let mut found = false;
    for dep in first_deps {
        if !second_deps.contains(&dep) {
            trail.push(strip_source(dep));
            result.push(trail.clone());
            trail.pop();
            found = true;
        }
    }
    for dep in second_deps {
        if !first_deps.contains(&dep) {
            trail.push(strip_source(dep));
            result.push(trail.clone());
            trail.pop();
            found = true;
        }
    }
    if !found {
        // The two are identical so far, recurse into deps.
        for dep in first_deps {
            diff(
                modified_members,
                first_resolve,
                second_resolve,
                dep,
                trail,
                result,
            );
        }
    }
    trail.pop();
}
