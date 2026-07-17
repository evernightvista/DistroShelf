use gtk::glib;
use gtk::prelude::*;
use serde::Serialize;

const USER_VISIBLE_STRING_PROPS: &[&str] = &[
    "label",
    "text",
    "title",
    "subtitle",
    "description",
    "placeholder-text",
    "tooltip-text",
    "tooltip-markup",
    "secondary-text",
    "heading",
    "body",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StringEntry {
    pub source: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WidgetText {
    pub path: Vec<usize>,
    pub type_name: String,
    pub accessible_role: String,
    pub strings: Vec<StringEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WidgetTextSnapshot {
    pub root_type: String,
    pub entries: Vec<WidgetText>,
}

pub fn extract_widget_text(root: &impl IsA<gtk::Widget>) -> WidgetTextSnapshot {
    let root_widget = root.upcast_ref::<gtk::Widget>();
    let mut entries = Vec::new();
    walk(root_widget, &mut Vec::new(), &mut entries);
    WidgetTextSnapshot {
        root_type: root_widget.type_().name().to_string(),
        entries,
    }
}

fn walk(widget: &gtk::Widget, path: &mut Vec<usize>, out: &mut Vec<WidgetText>) {
    if let Some(entry) = extract_one(widget, path) {
        out.push(entry);
    }
    let mut child = widget.first_child();
    let mut idx = 0;
    while let Some(c) = child {
        path.push(idx);
        walk(&c, path, out);
        path.pop();
        child = c.next_sibling();
        idx += 1;
    }
}

fn extract_one(widget: &gtk::Widget, path: &[usize]) -> Option<WidgetText> {
    let mut strings: Vec<StringEntry> = Vec::new();

    for pspec in widget.list_properties() {
        // `property_value` panics on write-only paramspecs, so the READABLE
        // guard is mandatory rather than cosmetic.
        if !pspec.flags().contains(glib::ParamFlags::READABLE) {
            continue;
        }
        let name = pspec.name();
        if !USER_VISIBLE_STRING_PROPS.contains(&name) {
            continue;
        }
        if pspec.value_type() != glib::Type::STRING {
            continue;
        }
        let value = widget.property_value(name);
        let Ok(s) = value.get_owned::<String>() else {
            continue;
        };
        if s.trim().is_empty() {
            continue;
        }
        strings.push(StringEntry {
            source: name.to_string(),
            value: s,
        });
    }

    if strings.is_empty() {
        return None;
    }
    strings.sort_by(|a, b| a.source.cmp(&b.source));

    Some(WidgetText {
        path: path.to_vec(),
        type_name: widget.type_().name().to_string(),
        accessible_role: format!("{:?}", widget.accessible_role()),
        strings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gtk::test]
    fn extracts_labels_buttons_entries_and_action_rows() {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let label = gtk::Label::new(Some("Hello World"));
        root.append(&label);

        let button = gtk::Button::with_label("Click me");
        button.set_tooltip_text(Some("Click to do something"));
        root.append(&button);

        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("Type here"));
        root.append(&entry);

        let row = adw::ActionRow::builder()
            .title("My Row")
            .subtitle("My Subtitle")
            .build();
        root.append(&row);

        let snapshot = extract_widget_text(&root);

        assert_eq!(snapshot.root_type, "GtkBox");

        let all_pairs: Vec<(String, String)> = snapshot
            .entries
            .iter()
            .flat_map(|w| w.strings.iter().cloned())
            .map(|s| (s.source, s.value))
            .collect();

        // Each pair must appear under its expected source name, not just
        // somewhere in the snapshot — guards against a regression that
        // mislabels every string.
        for (expected_source, expected_value) in [
            ("label", "Hello World"),
            ("label", "Click me"),
            ("tooltip-text", "Click to do something"),
            ("placeholder-text", "Type here"),
            ("title", "My Row"),
            ("subtitle", "My Subtitle"),
        ] {
            let needle = (expected_source.to_string(), expected_value.to_string());
            assert!(
                all_pairs.contains(&needle),
                "expected ({expected_source:?}, {expected_value:?}) in {all_pairs:?}"
            );
        }
    }

    #[gtk::test]
    fn root_widget_strings_use_empty_path() {
        let window = gtk::Window::new();
        window.set_title(Some("Window Title"));

        let snapshot = extract_widget_text(&window);

        let root_entry = snapshot
            .entries
            .iter()
            .find(|e| e.path.is_empty())
            .expect("root window should produce an entry with empty path");
        assert!(root_entry
            .strings
            .iter()
            .any(|s| s.source == "title" && s.value == "Window Title"));
    }

    #[gtk::test]
    fn skips_widgets_with_no_user_visible_text() {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&gtk::Box::new(gtk::Orientation::Horizontal, 0));

        let snapshot = extract_widget_text(&root);

        assert_eq!(snapshot.root_type, "GtkBox");
        assert!(
            snapshot.entries.is_empty(),
            "expected no entries, got {:?}",
            snapshot.entries
        );
    }

    #[gtk::test]
    fn path_is_zero_based_index_path() {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&gtk::Label::new(Some("first")));
        root.append(&gtk::Label::new(Some("second")));

        let snapshot = extract_widget_text(&root);

        let paths: Vec<&Vec<usize>> = snapshot.entries.iter().map(|e| &e.path).collect();
        assert!(paths.contains(&&vec![0]), "missing [0] in {paths:?}");
        assert!(paths.contains(&&vec![1]), "missing [1] in {paths:?}");
    }
}
