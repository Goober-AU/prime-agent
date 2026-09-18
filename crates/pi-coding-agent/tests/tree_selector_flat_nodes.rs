use pi_coding_agent::modes::agent_connection::types::{
    AgentConnectionSessionEntry, AgentConnectionSessionTreeNode,
};
use pi_coding_agent::modes::interactive::components::tree_selector::TreeList;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;

fn chain(length: usize) -> Vec<AgentConnectionSessionTreeNode> {
    let mut child: Option<AgentConnectionSessionTreeNode> = None;
    for index in (0..length).rev() {
        let id = format!("entry-{index}");
        let parent = (index > 0).then(|| format!("entry-{}", index - 1));
        let mut node = AgentConnectionSessionTreeNode {
            entry: AgentConnectionSessionEntry::Compaction {
                id,
                parent_id: parent,
                timestamp: format!("2026-09-18T00:00:{:02}.000Z", index % 60),
                summary: "fixture".to_string(),
                first_kept_entry_id: "entry-0".to_string(),
                tokens_before: 1.0,
                details: None,
                from_hook: None,
            },
            label: None,
            label_timestamp: None,
            children: Vec::new(),
        };
        if let Some(child) = child.take() {
            node.children.push(child);
        }
        child = Some(node);
    }
    child.into_iter().collect()
}

/// The TypeScript `FlatNode.node` is a shared reference. Owning a deep clone per
/// row retained one subtree copy per entry: quadratic on long chains, which is
/// what made opening the tree selector on a long session exhaust memory.
#[test]
fn flatten_tree_rows_do_not_retain_their_subtrees() {
    init_theme(None, false);
    let roots = chain(2_000);
    let mut list = TreeList::new(&roots, None, 20, Some("entry-0".to_string()), None);
    let selected = list.get_selected_node().expect("root row is selected");
    assert_eq!(selected.entry.id(), "entry-0");
    assert!(
        selected.children.is_empty(),
        "flattened rows must not retain their subtrees"
    );
    let frame = list.render(80.0);
    assert!(!frame.is_empty(), "the selector must render its visible window");
}
