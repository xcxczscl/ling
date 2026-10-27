use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use ling_sdk::{json, Value};

use crate::context::KernelCore;
use crate::error::KernelError;
use crate::kernel;
use crate::loader;
use crate::manifest::{self, Manifest};
use crate::state::Shared;

/// 调和计划的一步（`docs/03` §3.2；事件载荷见 `docs/04` §5）。
#[derive(Debug, Clone)]
pub struct PlanStep {
    pub action: &'static str,
    pub plugin: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// 一次配置调和的结果。
#[derive(Debug, Clone)]
pub struct ReconcileReport {
    pub plan_id: String,
    pub steps: Vec<PlanStep>,
    pub error: Option<String>,
}

impl ReconcileReport {
    pub fn ok(&self) -> bool {
        self.error.is_none() && self.steps.iter().all(|s| s.ok)
    }
}

/// 版本匹配（`docs/03` §1.2）：`id` 任意；`id@1` 主版本；`id@1.2`
/// 主+次；`id@1.2.3` 精确——按段前缀相等判定。
pub fn version_satisfied(spec: &str, version: &str) -> bool {
    match spec.split_once('@') {
        None => true,
        Some((_, want)) => {
            let want_parts: Vec<&str> = want.split('.').collect();
            let have_parts: Vec<&str> = version.split('.').collect();
            if want_parts.is_empty() || want_parts.len() > have_parts.len() {
                return false;
            }
            want_parts
                .iter()
                .zip(have_parts.iter())
                .all(|(a, b)| a == b)
        }
    }
}

struct Node {
    version: String,
    requires: Vec<String>,
}

fn installed_nodes(shared: &Shared) -> Result<BTreeMap<String, Node>, KernelError> {
    let mut nodes = BTreeMap::new();
    for id in loader::installed_ids(shared) {
        let m: Manifest = manifest::read_manifest(&shared.plugin_dir(&id))?;
        nodes.insert(
            id,
            Node {
                version: m.plugin.version,
                requires: m.requires.plugins.clone(),
            },
        );
    }
    Ok(nodes)
}

fn dep_id(spec: &str) -> &str {
    spec.split('@').next().unwrap_or(spec)
}

fn edges(nodes: &BTreeMap<String, Node>) -> BTreeMap<String, Vec<String>> {
    nodes
        .iter()
        .map(|(id, node)| {
            let deps: Vec<String> = node
                .requires
                .iter()
                .map(|s| dep_id(s).to_string())
                .filter(|d| nodes.contains_key(d))
                .collect();
            (id.clone(), deps)
        })
        .collect()
}

fn find_cycle(nodes: &BTreeMap<String, Node>) -> Option<Vec<String>> {
    let edges = edges(nodes);
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    fn dfs(
        id: &str,
        edges: &BTreeMap<String, Vec<String>>,
        marks: &mut HashMap<String, Mark>,
        stack: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        marks.insert(id.to_string(), Mark::Visiting);
        stack.push(id.to_string());
        for dep in edges.get(id).into_iter().flatten() {
            match marks.get(dep) {
                Some(Mark::Visiting) => {
                    let start = stack.iter().position(|p| p == dep).unwrap_or(0);
                    let mut cycle: Vec<String> = stack[start..].to_vec();
                    cycle.push(dep.clone());
                    return Some(cycle);
                }
                Some(Mark::Done) => {}
                None => {
                    if let Some(c) = dfs(dep, edges, marks, stack) {
                        return Some(c);
                    }
                }
            }
        }
        stack.pop();
        marks.insert(id.to_string(), Mark::Done);
        None
    }
    let mut marks = HashMap::new();
    for id in nodes.keys() {
        if !marks.contains_key(id) {
            let mut stack = Vec::new();
            if let Some(c) = dfs(id, &edges, &mut marks, &mut stack) {
                return Some(c);
            }
        }
    }
    None
}

fn requirement_met(
    spec: &str,
    nodes: &BTreeMap<String, Node>,
    disabled: &BTreeSet<String>,
    cache: &mut HashMap<String, bool>,
) -> bool {
    let dep = dep_id(spec);
    let Some(node) = nodes.get(dep) else {
        return false;
    };
    if !version_satisfied(spec, &node.version) {
        return false;
    }
    satisfiable(dep, nodes, disabled, cache)
}

fn satisfiable(
    id: &str,
    nodes: &BTreeMap<String, Node>,
    disabled: &BTreeSet<String>,
    cache: &mut HashMap<String, bool>,
) -> bool {
    if disabled.contains(id) {
        return false;
    }
    if let Some(v) = cache.get(id) {
        return *v;
    }
    let node = &nodes[id];
    let ok = node
        .requires
        .iter()
        .all(|spec| requirement_met(spec, nodes, disabled, cache));
    cache.insert(id.to_string(), ok);
    ok
}

fn topo_order(nodes: &BTreeMap<String, Node>) -> Vec<String> {
    let edges = edges(nodes);
    let mut indegree: BTreeMap<String, usize> = nodes
        .keys()
        .map(|id| {
            let deps = edges.get(id).map(|d| d.len()).unwrap_or(0);
            (id.clone(), deps)
        })
        .collect();
    let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, deps) in &edges {
        for dep in deps {
            dependents.entry(dep.clone()).or_default().push(id.clone());
        }
    }
    let mut queue: Vec<String> = indegree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(id, _)| id.clone())
        .collect();
    queue.sort();
    let mut order = Vec::new();
    while let Some(id) = queue.first().cloned() {
        queue.remove(0);
        order.push(id.clone());
        for dependent in dependents.get(&id).into_iter().flatten() {
            let d = indegree.get_mut(dependent).expect("节点存在");
            *d -= 1;
            if *d == 0 {
                queue.push(dependent.clone());
                queue.sort();
            }
        }
    }
    order
}

pub fn check_no_cycle(shared: &Shared) -> Result<(), KernelError> {
    let nodes = installed_nodes(shared)?;
    match find_cycle(&nodes) {
        Some(cycle) => Err(KernelError::dependency_cycle(cycle)),
        None => Ok(()),
    }
}

/// id 的**已激活**传递依赖方（"因它存在才激活"的下游），反向拓扑序（最远的依赖者在前）。
pub fn active_dependents(shared: &Shared, id: &str) -> Result<Vec<String>, KernelError> {
    let nodes = installed_nodes(shared)?;
    let edges = edges(&nodes);
    let mut reverse: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (from, deps) in &edges {
        for dep in deps {
            reverse.entry(dep.clone()).or_default().push(from.clone());
        }
    }
    let mut dependents = BTreeSet::new();
    let mut stack = vec![id.to_string()];
    while let Some(cur) = stack.pop() {
        for dep in reverse.get(&cur).into_iter().flatten() {
            if dependents.insert(dep.clone()) {
                stack.push(dep.clone());
            }
        }
    }
    let order = topo_order(&nodes);
    let mut ordered: Vec<String> = order
        .into_iter()
        .filter(|p| dependents.contains(p) && is_active(shared, p))
        .collect();
    ordered.reverse();
    Ok(ordered)
}

fn is_active(shared: &Shared, id: &str) -> bool {
    shared.plugins.read().unwrap().contains_key(id)
}

/// 优雅停机顺序（`docs/01` §4）：已激活插件的逆向拓扑序（最依赖前者先停）。
pub fn shutdown_order(shared: &Shared) -> Result<Vec<String>, KernelError> {
    let nodes = installed_nodes(shared)?;
    let mut order: Vec<String> = topo_order(&nodes)
        .into_iter()
        .filter(|id| is_active(shared, id))
        .collect();
    order.reverse();
    Ok(order)
}

fn next_plan_id(shared: &Shared) -> String {
    let seq = shared
        .plan_seq
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}-{seq:03}")
}

fn plan_step_event(
    shared: &Shared,
    plan_id: &str,
    step: usize,
    action: &str,
    plugin: &str,
    ok: bool,
    error: Option<&str>,
) {
    let mut payload = json!({
        "plan_id": plan_id,
        "step": step,
        "action": action,
        "plugin": plugin,
        "ok": ok,
    });
    if let Some(e) = error {
        payload["error"] = Value::String(e.to_string());
    }
    shared.dispatch("kernel.plan.step", payload);
}

/// 配置调和（`docs/03` §4）：重建依赖图 → 校验无环 → 判定 → 拓扑序逐步执行。
/// 任何一步失败 → 回滚已执行步骤，报告失败。调用方需持有 `config_gate`。
pub fn reconcile(
    shared: &Arc<Shared>,
    core: &Arc<KernelCore>,
) -> Result<ReconcileReport, KernelError> {
    let nodes = installed_nodes(shared)?;
    if let Some(cycle) = find_cycle(&nodes) {
        return Err(KernelError::dependency_cycle(cycle));
    }
    let disabled = shared.disabled.read().unwrap().clone();
    let mut cache = HashMap::new();
    let should: BTreeMap<String, bool> = nodes
        .keys()
        .map(|id| (id.clone(), satisfiable(id, &nodes, &disabled, &mut cache)))
        .collect();

    let order = topo_order(&nodes);
    let mut step_list: Vec<(&'static str, String)> = Vec::new();
    for id in order.iter().rev() {
        if is_active(shared, id) && !should[id] {
            step_list.push(("deactivate", id.clone()));
        }
    }
    for id in &order {
        if should[id] && !is_active(shared, id) {
            step_list.push(("activate", id.clone()));
        }
    }

    let plan_id = next_plan_id(shared);
    let mut steps = Vec::new();
    let mut executed: Vec<(&'static str, String)> = Vec::new();
    for (index, (action, id)) in step_list.iter().enumerate() {
        let result = if *action == "activate" {
            loader::load(shared, core, id)
        } else {
            kernel::stop_plugin(shared, id).map(|_| ())
        };
        let error = result.err().map(|e| e.to_string());
        let ok = error.is_none();
        if ok && *action == "deactivate" {
            loader::state_event(shared, id, "deactivated");
        }
        plan_step_event(
            shared,
            &plan_id,
            index + 1,
            action,
            id,
            ok,
            error.as_deref(),
        );
        steps.push(PlanStep {
            action,
            plugin: id.clone(),
            ok,
            error: error.clone(),
        });
        if ok {
            executed.push((action, id.clone()));
        } else {
            let mut rolled_back = 0usize;
            for (old_action, old_id) in executed.iter().rev() {
                let compensated = if *old_action == "activate" {
                    kernel::stop_plugin(shared, old_id).map(|_| ())
                } else {
                    loader::load(shared, core, old_id)
                };
                if compensated.is_ok() {
                    if *old_action == "activate" {
                        loader::state_event(shared, old_id, "deactivated");
                    }
                    rolled_back += 1;
                }
            }
            return Ok(ReconcileReport {
                plan_id,
                steps,
                error: Some(format!(
                    "步骤 {action} {id} 失败，已回滚 {rolled_back}/{} 个已执行步骤",
                    executed.len()
                )),
            });
        }
    }
    Ok(ReconcileReport {
        plan_id,
        steps,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::{find_cycle, version_satisfied};
    use std::collections::BTreeMap;

    #[test]
    fn version_spec_matches_by_prefix() {
        assert!(version_satisfied("demo-b", "9.9.9"));
        assert!(version_satisfied("demo-b@1", "1.0.0"));
        assert!(!version_satisfied("demo-b@1", "2.0.0"));
        assert!(version_satisfied("demo-b@1.2", "1.2.0"));
        assert!(!version_satisfied("demo-b@1.2", "1.3.0"));
        assert!(version_satisfied("demo-b@1.2.3", "1.2.3"));
        assert!(!version_satisfied("demo-b@1.2.3", "1.2.4"));
        assert!(!version_satisfied("demo-b@1.2.3", "1.2"));
    }

    #[test]
    fn cycle_detection_reports_closed_path() {
        let nodes = BTreeMap::from([
            (
                "a".to_string(),
                super::Node {
                    version: "0.1.0".to_string(),
                    requires: vec!["b@0.1.0".to_string()],
                },
            ),
            (
                "b".to_string(),
                super::Node {
                    version: "0.1.0".to_string(),
                    requires: vec!["a@0.1.0".to_string()],
                },
            ),
            (
                "c".to_string(),
                super::Node {
                    version: "0.1.0".to_string(),
                    requires: vec![],
                },
            ),
        ]);
        let cycle = find_cycle(&nodes).expect("存在环");
        assert_eq!(cycle.first(), cycle.last(), "环闭合：首尾相同");
        let set: std::collections::BTreeSet<String> = cycle.iter().cloned().collect();
        assert!(set.contains("a") && set.contains("b") && !set.contains("c"));
        assert!(find_cycle(&BTreeMap::from([(
            "c".to_string(),
            super::Node {
                version: "0.1.0".to_string(),
                requires: vec!["a@0.1.0".to_string()],
            },
        )]))
        .is_none());
    }
}
