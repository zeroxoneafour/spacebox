//! Structural JSON diffs between two state snapshots.
//!
//! A diff is a list of operations applied in order. `p` is a path of object keys (strings)
//! and array indices (numbers):
//!
//! ```text
//! { "p": [...], "v": value }   set the value at p (creating it if missing)
//! { "p": [...], "d": 1 }       delete the object key at p
//! { "p": [...], "n": len }     resize the array at p to len (before any following sets)
//! ```
//!
//! Arrays of plain scalars (resource vectors, id lists) are replaced wholesale when they
//! change, which is smaller than per-element ops for the dense changes they usually see.

use serde_json::{Value, json};

use crate::snapshot::Snapshot;

#[cfg(test)]
pub fn diff(old: &Value, new: &Value) -> Vec<Value> {
    let mut ops = Vec::new();
    let mut path = Vec::new();
    walk(old, new, &mut path, &mut ops);
    ops
}

/// The ops turning one snapshot into another. Only entities whose text changed are parsed and
/// compared structurally, so the result is the same as diffing the full states.
pub fn diff_snapshots(old: &Snapshot, new: &Snapshot) -> Vec<Value> {
    let mut ops = Vec::new();
    let mut path = Vec::new();
    // The top level holds a few small fields besides the lists, which never vanish.
    if let (Value::Object(a), Value::Object(b)) = (&old.top, &new.top) {
        for (k, vb) in b {
            path.push(Value::String(k.clone()));
            match a.get(k) {
                Some(va) => walk(va, vb, &mut path, &mut ops),
                None => ops.push(set(&path, vb)),
            }
            path.pop();
        }
    }
    let parse = |s: &str| serde_json::from_str::<Value>(s).expect("snapshot text is JSON");
    for ((name, a), (_, b)) in old.lists.iter().zip(&new.lists) {
        path.push(Value::String((*name).to_string()));
        if a.len() != b.len() {
            ops.push(json!({ "p": path, "n": b.len() }));
        }
        for (i, vb) in b.iter().enumerate() {
            if a.get(i)
                .is_some_and(|va| std::sync::Arc::ptr_eq(va, vb) || va == vb)
            {
                continue;
            }
            path.push(Value::from(i));
            let vb = parse(vb);
            match a.get(i) {
                Some(va) => walk(&parse(va), &vb, &mut path, &mut ops),
                None => ops.push(set(&path, &vb)),
            }
            path.pop();
        }
        path.pop();
    }
    ops
}

fn is_scalar(v: &Value) -> bool {
    !matches!(v, Value::Array(_) | Value::Object(_))
}

fn set(path: &[Value], v: &Value) -> Value {
    json!({ "p": path, "v": v })
}

fn walk(a: &Value, b: &Value, path: &mut Vec<Value>, ops: &mut Vec<Value>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, vb) in y {
                path.push(Value::String(k.clone()));
                match x.get(k) {
                    Some(va) => walk(va, vb, path, ops),
                    None => ops.push(set(path, vb)),
                }
                path.pop();
            }
            for k in x.keys().filter(|k| !y.contains_key(*k)) {
                path.push(Value::String(k.clone()));
                ops.push(json!({ "p": path, "d": 1 }));
                path.pop();
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if y.iter().all(is_scalar) {
                if x != y {
                    ops.push(set(path, b));
                }
                return;
            }
            if x.len() != y.len() {
                ops.push(json!({ "p": path, "n": y.len() }));
            }
            for (i, vb) in y.iter().enumerate() {
                path.push(Value::from(i));
                match x.get(i) {
                    Some(va) => walk(va, vb, path, ops),
                    None => ops.push(set(path, vb)),
                }
                path.pop();
            }
        }
        _ => {
            if a != b {
                ops.push(set(path, b));
            }
        }
    }
}

/// Applies a diff in place (mirrors the client implementation; used for tests).
#[cfg(test)]
pub fn apply(target: &mut Value, ops: &[Value]) {
    for op in ops {
        let path = op["p"].as_array().unwrap();
        if path.is_empty() {
            if let Some(v) = op.get("v") {
                *target = v.clone();
            }
            continue;
        }
        let mut cur = &mut *target;
        for step in &path[..path.len() - 1] {
            cur = match step {
                Value::String(k) => &mut cur[k.as_str()],
                n => &mut cur[n.as_u64().unwrap() as usize],
            };
        }
        let last = &path[path.len() - 1];
        if let Some(v) = op.get("v") {
            match last {
                Value::String(k) => {
                    cur.as_object_mut().unwrap().insert(k.clone(), v.clone());
                }
                n => {
                    let i = n.as_u64().unwrap() as usize;
                    let arr = cur.as_array_mut().unwrap();
                    if i >= arr.len() {
                        arr.resize(i + 1, Value::Null);
                    }
                    arr[i] = v.clone();
                }
            }
        } else if op.get("d").is_some() {
            cur.as_object_mut().unwrap().remove(last.as_str().unwrap());
        } else if let Some(n) = op.get("n") {
            let arr = match last {
                Value::String(k) => &mut cur[k.as_str()],
                i => &mut cur[i.as_u64().unwrap() as usize],
            };
            arr.as_array_mut()
                .unwrap()
                .resize(n.as_u64().unwrap() as usize, Value::Null);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(a: Value, b: Value) -> usize {
        let ops = diff(&a, &b);
        let mut c = a.clone();
        apply(&mut c, &ops);
        assert_eq!(c, b, "ops: {ops:?}");
        ops.len()
    }

    #[test]
    fn identical_values_produce_no_ops() {
        let v = json!({"a": [1, 2, {"b": 3}], "c": "x"});
        assert_eq!(roundtrip(v.clone(), v), 0);
    }

    #[test]
    fn nested_changes_are_minimal() {
        let a = json!({"planets": [{"pop": 1, "name": "a"}, {"pop": 2, "name": "b"}]});
        let b = json!({"planets": [{"pop": 1, "name": "a"}, {"pop": 3, "name": "b"}]});
        let ops = diff(&a, &b);
        assert_eq!(ops, vec![json!({"p": ["planets", 1, "pop"], "v": 3})]);
        roundtrip(a, b);
    }

    #[test]
    fn arrays_grow_shrink_and_keys_vanish() {
        roundtrip(
            json!({"s": [{"x": 1}]}),
            json!({"s": [{"x": 1}, {"x": 2}, {"x": 3}]}),
        );
        roundtrip(json!({"s": [{"x": 1}, {"x": 2}]}), json!({"s": [{"x": 5}]}));
        roundtrip(json!({"a": 1, "b": 2}), json!({"a": 1}));
        roundtrip(json!({"v": [1, 2, 3]}), json!({"v": [1, 9]}));
        roundtrip(json!({"x": null}), json!({"x": {"y": [1]}}));
    }

    #[test]
    fn snapshot_diffs_rebuild_the_new_state() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../data/scenarios/sol.toml");
        let (mut w, _) = spacebox_core::load_world(path).unwrap();
        w.refresh_topology();
        let a = crate::snapshot::state(&w, None);
        w.run(30);
        let b = crate::snapshot::state(&w, Some(&a));
        // Unchanged entities share their text with the previous version.
        assert!(
            a.lists[0]
                .1
                .iter()
                .zip(&b.lists[0].1)
                .any(|(x, y)| std::sync::Arc::ptr_eq(x, y))
        );
        let ops = diff_snapshots(&a, &b);
        assert!(!ops.is_empty());
        let mut full: Value = serde_json::from_str(&a.to_json()).unwrap();
        apply(&mut full, &ops);
        let want: Value = serde_json::from_str(&b.to_json()).unwrap();
        assert_eq!(full, want);
        assert!(diff_snapshots(&b, &b).is_empty());
    }
}
