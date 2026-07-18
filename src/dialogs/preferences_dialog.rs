use crate::backends::supported_terminals;
use crate::i18n::gettext;
use crate::models::{DialogType, DistroboxSource, RootStore};
use crate::query::LastFetch;
use crate::widgets::TerminalComboRow;

use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::{Properties, clone, derived_properties};
use gtk::{gio, glib};
use std::cell::RefCell;
use tracing::error;

mod imp {
    use super::*;

    #[derive(Properties)]
    #[properties(wrapper_type = super::PreferencesDialog)]
    pub struct PreferencesDialog {
        #[property(get, set, construct)]
        pub root_store: RefCell<RootStore>,
        pub terminal_combo_row: RefCell<Option<TerminalComboRow>>,
        pub delete_btn: gtk::Button,
        pub add_terminal_btn: gtk::Button,
        pub host_row: adw::ActionRow,
        pub bundled_row: adw::ActionRow,
        pub host_check: gtk::CheckButton,
        pub bundled_check: gtk::CheckButton,
        pub update_btn: gtk::Button,
        pub bundled_menu_model: gio::Menu,
        pub bundled_menu_button: gtk::MenuButton,
    }

    impl Default for PreferencesDialog {
        fn default() -> Self {
            Self {
                root_store: RefCell::new(RootStore::default()),
                terminal_combo_row: RefCell::new(None),
                delete_btn: gtk::Button::new(),
                add_terminal_btn: gtk::Button::new(),
                host_row: adw::ActionRow::new(),
                bundled_row: adw::ActionRow::new(),
                host_check: gtk::CheckButton::new(),
                bundled_check: gtk::CheckButton::new(),
                update_btn: gtk::Button::new(),
                bundled_menu_model: gio::Menu::new(),
                bundled_menu_button: gtk::MenuButton::new(),
            }
        }
    }

    #[derived_properties]
    impl ObjectImpl for PreferencesDialog {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            obj.set_title(&gettext("Preferences"));

            let page = adw::PreferencesPage::new();

            // Terminal Settings Group
            let terminal_group = adw::PreferencesGroup::new();
            terminal_group.set_title(&gettext("Terminal Settings"));

            // Initialize terminal combo row
            let terminal_combo_row = TerminalComboRow::new_with_params(obj.root_store());
            self.terminal_combo_row
                .replace(Some(terminal_combo_row.clone()));

            // Initialize delete button
            self.delete_btn.set_label(&gettext("Delete"));
            self.delete_btn.add_css_class("destructive-action");
            self.delete_btn.add_css_class("pill");

            // Set initial delete button state
            if let Some(selected) = terminal_combo_row.selected_item() {
                let selected_name = selected
                    .downcast_ref::<gtk::StringObject>()
                    .unwrap()
                    .string();
                let is_read_only = obj
                    .root_store()
                    .terminal_repository()
                    .is_read_only(&selected_name);

                self.delete_btn.set_sensitive(!is_read_only);
            }

            // Connect delete button
            self.delete_btn.connect_clicked(clone!(
                #[weak]
                obj,
                move |_| {
                    obj.handle_delete_terminal();
                }
            ));

            // Update delete button when selection changes
            terminal_combo_row.connect_selected_item_notify(clone!(
                #[weak]
                obj,
                move |_| {
                    obj.update_delete_button_state();
                }
            ));

            terminal_group.add(&terminal_combo_row);

            // Connect to terminals-changed signal to update UI when terminals change
            obj.root_store().terminal_repository().connect_closure(
                "terminals-changed",
                false,
                glib::closure_local!(
                    #[weak]
                    obj,
                    #[weak]
                    terminal_combo_row,
                    move |_: crate::backends::supported_terminals::TerminalRepository| {
                        terminal_combo_row.rebuild_terminals_list();
                        obj.update_delete_button_state();
                    }
                ),
            );

            // Initialize add terminal button
            self.add_terminal_btn.set_label(&gettext("Add Custom"));
            self.add_terminal_btn.add_css_class("pill");
            self.add_terminal_btn.set_halign(gtk::Align::Start);

            // Connect add terminal button
            self.add_terminal_btn.connect_clicked(clone!(
                #[weak]
                obj,
                move |_| {
                    obj.show_add_terminal_dialog();
                }
            ));

            let button_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            button_box.set_margin_start(12);
            button_box.set_margin_end(12);
            button_box.set_margin_top(12);
            button_box.set_margin_bottom(12);

            button_box.append(&self.delete_btn);
            button_box.append(&self.add_terminal_btn);
            terminal_group.add(&button_box);

            page.add(&terminal_group);

            // Distrobox Group (general settings)
            let settings = gio::Settings::new("com.ranfdev.DistroShelf");

            let distrobox_group = adw::PreferencesGroup::new();
            distrobox_group.set_title(&gettext("Distrobox"));

            let no_entry_row = adw::SwitchRow::new();
            no_entry_row.set_title(&gettext("Use --no-entry for new containers"));
            no_entry_row.set_subtitle(&gettext(
                "No .desktop app entry is created, so it won't appear in your app list.",
            ));
            no_entry_row.set_active(settings.boolean("distrobox-create-no-entry"));

            let settings_for_no_entry = settings.clone();
            no_entry_row.connect_active_notify(move |row| {
                let _ =
                    settings_for_no_entry.set_boolean("distrobox-create-no-entry", row.is_active());
            });

            distrobox_group.add(&no_entry_row);
            page.add(&distrobox_group);

            // Distrobox Version Group (source selection + management)
            let version_group = adw::PreferencesGroup::new();
            version_group.set_title(&gettext("Distrobox Version"));

            // Link the two check buttons into a mutually exclusive radio group.
            let host_check = self.host_check.clone();
            let bundled_check = self.bundled_check.clone();
            bundled_check.set_group(Some(&host_check));

            // ── System row ──────────────────────────────────────────────
            let host_row = self.host_row.clone();
            host_row.set_title(&gettext("Host Version"));
            host_row.add_prefix(&host_check);
            host_row.set_activatable_widget(Some(&host_check));

            obj.root_store()
                .host_distrobox_version()
                .connect_success(clone!(
                    #[weak(rename_to = this)]
                    obj,
                    move |_| {
                        this.refresh_host_row();
                    }
                ));
            obj.root_store()
                .host_distrobox_version()
                .connect_error(clone!(
                    #[weak(rename_to = this)]
                    obj,
                    move |_| {
                        this.refresh_host_row();
                    }
                ));

            version_group.add(&host_row);

            // ── Bundled row ─────────────────────────────────────────────
            // The radio selects the source; the menu manages the download.
            let bundled_row = self.bundled_row.clone();
            bundled_row.set_title(&gettext("Bundled Version"));
            bundled_row.add_prefix(&bundled_check);
            bundled_row.set_activatable_widget(Some(&bundled_check));

            // Update button: shown only when a newer bundled version is
            // available. It's the prominent affordance for upgrading.
            let update_btn = self.update_btn.clone();
            update_btn.set_icon_name("software-update-available-symbolic");
            update_btn.set_tooltip_text(Some(&gettext("Update bundled distrobox")));
            update_btn.set_valign(gtk::Align::Center);
            update_btn.add_css_class("suggested-action");
            update_btn.add_css_class("circular");
            update_btn.connect_clicked(clone!(
                #[weak]
                obj,
                move |_| {
                    obj.root_store().download_distrobox();
                    obj.root_store().set_current_dialog(DialogType::TaskManager);
                }
            ));
            bundled_row.add_suffix(&update_btn);

            let menu_button = self.bundled_menu_button.clone();
            menu_button.set_icon_name("view-more-symbolic");
            menu_button.set_tooltip_text(Some(&gettext("Bundled distrobox actions")));
            menu_button.set_valign(gtk::Align::Center);
            menu_button.add_css_class("flat");
            let popover = gtk::PopoverMenu::from_model(Some(&self.bundled_menu_model));
            menu_button.set_popover(Some(&popover));
            bundled_row.add_suffix(&menu_button);

            version_group.add(&bundled_row);

            // Radio → setting
            host_check.connect_toggled(clone!(
                #[weak(rename_to = this)]
                obj,
                move |cb| {
                    if cb.is_active() {
                        this.root_store()
                            .set_distrobox_source(DistroboxSource::Host);
                    }
                }
            ));
            bundled_check.connect_toggled(clone!(
                #[weak(rename_to = this)]
                obj,
                move |cb| {
                    if cb.is_active() {
                        this.root_store()
                            .set_distrobox_source(DistroboxSource::Bundled);
                    }
                }
            ));

            // Initial render of both rows + the radio selection
            obj.refresh_host_row();
            obj.refresh_bundled_row();
            obj.sync_selection();

            // Keep the bundled row fresh when update availability changes
            obj.root_store()
                .connect_bundled_update_available_notify(clone!(
                    #[weak(rename_to = this)]
                    obj,
                    move |_| {
                        this.refresh_bundled_row();
                    }
                ));

            // Refresh after bundled query completes or download finishes
            obj.root_store()
                .bundled_distrobox_version()
                .connect_success(clone!(
                    #[weak(rename_to = this)]
                    obj,
                    move |_| {
                        this.refresh_bundled_row();
                    }
                ));

            // Keep the radios in sync when the setting changes elsewhere
            obj.root_store().settings().connect_changed(
                Some("distrobox-executable"),
                clone!(
                    #[weak(rename_to = this)]
                    obj,
                    move |_, _| {
                        this.sync_selection();
                    }
                ),
            );

            page.add(&version_group);
            obj.add(&page);
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreferencesDialog {
        const NAME: &'static str = "PreferencesDialog";
        type Type = super::PreferencesDialog;
        type ParentType = adw::PreferencesDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.install_action(
                "dialog.download-distrobox",
                None,
                |this, _action, _target| {
                    this.root_store().download_distrobox();
                    this.root_store()
                        .set_current_dialog(DialogType::TaskManager);
                },
            );
        }
    }

    impl WidgetImpl for PreferencesDialog {}
    impl AdwDialogImpl for PreferencesDialog {}
    impl PreferencesDialogImpl for PreferencesDialog {}
}

glib::wrapper! {
    pub struct PreferencesDialog(ObjectSubclass<imp::PreferencesDialog>)
        @extends adw::PreferencesDialog, adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Actionable;
}

impl PreferencesDialog {
    pub fn new(root_store: RootStore) -> Self {
        let this = glib::Object::builder()
            .property("root-store", root_store.clone())
            .build();

        root_store
            .terminal_repository()
            .flatpak_terminals_query()
            .refetch();
        this
    }

    /// Refresh the host distrobox row subtitle (version · path) from the
    /// latest query state. The row is greyed out when no version is detected,
    /// so an unavailable host distrobox can't be selected.
    fn refresh_host_row(&self) {
        let imp = self.imp();
        let query = self.root_store().host_distrobox_version();
        let (subtitle, has_version) = match query.last_fetch() {
            LastFetch::Success => match query.data() {
                Some(Some(info)) => (format!("{} · {}", info.version, info.path), true),
                _ => (gettext("Not available on this system"), false),
            },
            LastFetch::Error => (gettext("Not available on this system"), false),
            LastFetch::Pending => ("—".to_string(), true),
        };
        imp.host_row.set_subtitle(&subtitle);
        imp.host_row.set_sensitive(has_version);
    }

    /// Refresh the bundled distrobox row: subtitle (version/path/status) and
    /// the context-aware action menu (download / update / re-download).
    fn refresh_bundled_row(&self) {
        let imp = self.imp();
        let query = self.root_store().bundled_distrobox_version();
        let is_installed = matches!(query.data(), Some(Some(_)));
        let update_available = self.root_store().bundled_update_available();

        let subtitle = match query.data() {
            Some(Some(info)) => {
                if update_available {
                    format!(
                        "{} · {} · {}",
                        info.version,
                        info.path,
                        gettext("Update available")
                    )
                } else {
                    format!("{} · {}", info.version, info.path)
                }
            }
            _ => crate::gettext_f!(
                "Not installed · {version} available",
                "version" => crate::distrobox_downloader::DISTROBOX_VERSION,
            ),
        };
        imp.bundled_row.set_subtitle(&subtitle);

        // The update button is the dedicated upgrade affordance; the menu only
        // holds download / re-download.
        let show_update_btn = is_installed && update_available;
        imp.update_btn.set_visible(show_update_btn);
        if show_update_btn {
            imp.update_btn.set_tooltip_text(Some(&format!(
                "{} ({})",
                gettext("Update bundled distrobox"),
                crate::distrobox_downloader::DISTROBOX_VERSION,
            )));
        }

        imp.bundled_menu_model.remove_all();
        if !is_installed {
            imp.bundled_menu_model.append(
                Some(&gettext("Download")),
                Some("dialog.download-distrobox"),
            );
        } else {
            imp.bundled_menu_model.append(
                Some(&gettext("Re-download")),
                Some("dialog.download-distrobox"),
            );
        }
    }

    /// Sync the radio buttons to the current `distrobox-executable` setting.
    fn sync_selection(&self) {
        let is_bundled = self.root_store().distrobox_source() == DistroboxSource::Bundled;
        let imp = self.imp();
        imp.bundled_check.set_active(is_bundled);
        imp.host_check.set_active(!is_bundled);
    }

    fn update_delete_button_state(&self) {
        let imp = self.imp();
        if let (Some(terminal_combo_row), Some(delete_btn)) = (
            imp.terminal_combo_row.borrow().as_ref(),
            Some(&imp.delete_btn),
        ) && let Some(selected) = terminal_combo_row.selected_item()
        {
            let selected_name = selected
                .downcast_ref::<gtk::StringObject>()
                .unwrap()
                .string();
            let is_read_only = self
                .root_store()
                .terminal_repository()
                .is_read_only(&selected_name);

            delete_btn.set_sensitive(!is_read_only);
        }
    }

    fn handle_delete_terminal(&self) {
        let imp = self.imp();
        let terminal_combo_row = match imp.terminal_combo_row.borrow().as_ref() {
            Some(row) => row.clone(),
            None => return,
        };

        let selected = terminal_combo_row
            .selected_item()
            .and_downcast_ref::<gtk::StringObject>()
            .unwrap()
            .string();

        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Delete this terminal?"))
            .body(gettext("This terminal will be removed from the terminal list. This action cannot be undone."))
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_response("cancel", &gettext("Cancel"));
        dialog.add_response("delete", &gettext("Delete"));

        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.connect_response(
            Some("delete"),
            clone!(
                #[weak(rename_to = this)]
                self,
                #[strong]
                selected,
                move |d, _| {
                    match this
                        .root_store()
                        .terminal_repository()
                        .delete_terminal(&selected)
                    {
                        Ok(_) => {
                            glib::MainContext::ref_thread_default().spawn_local(async move {
                                let terminal_combo_row =
                                    this.imp().terminal_combo_row.borrow().as_ref().cloned();
                                if let Some(terminal_combo_row) = terminal_combo_row {
                                    terminal_combo_row.rebuild_terminals_list();
                                    terminal_combo_row.set_selected_by_name(
                                        &this
                                            .root_store()
                                            .terminal_repository()
                                            .default_terminal()
                                            .await
                                            .map(|x| x.name)
                                            .unwrap_or_default(),
                                    );
                                }

                                this.add_toast(adw::Toast::new(&gettext(
                                    "Terminal removed successfully",
                                )));
                            });
                        }
                        Err(err) => {
                            error!(error = %err, "Failed to delete terminal");
                            this.add_toast(adw::Toast::new(&gettext("Failed to delete terminal")));
                        }
                    }
                    d.close();
                }
            ),
        );

        dialog.present(Some(self));
    }

    fn show_add_terminal_dialog(&self) {
        let custom_dialog = adw::Dialog::new();
        custom_dialog.set_title(&gettext("Add Custom Terminal"));

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&adw::HeaderBar::new());

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_top(12);
        content.set_margin_bottom(12);

        let group = adw::PreferencesGroup::new();

        // Name entry
        let name_entry = adw::EntryRow::builder()
            .title(gettext("Terminal Name"))
            .build();

        // Program entry
        let program_entry = adw::EntryRow::builder()
            .title(gettext("Program Path"))
            .build();

        // Separator argument entry
        let separator_entry = adw::EntryRow::builder()
            .title(gettext("Separator Argument"))
            .build();

        group.add(&name_entry);
        group.add(&program_entry);
        group.add(&separator_entry);
        content.append(&group);

        // Add note about separator
        let info_label = gtk::Label::new(Some(&gettext(
            "The separator argument is used to pass commands to the terminal.\nExamples: '--' for GNOME Terminal, '-e' for xterm",
        )));
        info_label.add_css_class("caption");
        info_label.add_css_class("dim-label");
        info_label.set_wrap(true);
        info_label.set_xalign(0.0);
        info_label.set_margin_start(12);
        content.append(&info_label);

        // Buttons
        let button_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        button_box.set_margin_top(12);
        button_box.set_homogeneous(true);

        let cancel_btn = gtk::Button::with_label(&gettext("Cancel"));
        cancel_btn.add_css_class("pill");

        let save_btn = gtk::Button::with_label(&gettext("Save"));
        save_btn.add_css_class("suggested-action");
        save_btn.add_css_class("pill");

        button_box.append(&cancel_btn);
        button_box.append(&save_btn);
        content.append(&button_box);

        toolbar_view.set_content(Some(&content));
        custom_dialog.set_child(Some(&toolbar_view));

        // Connect button handlers
        cancel_btn.connect_clicked(clone!(
            #[weak]
            custom_dialog,
            move |_| {
                custom_dialog.close();
            }
        ));

        save_btn.connect_clicked(clone!(
            #[weak]
            custom_dialog,
            #[weak]
            name_entry,
            #[weak]
            program_entry,
            #[weak]
            separator_entry,
            #[weak(rename_to = this)]
            self,
            move |_| {
                let name = name_entry.text().to_string();
                let program = program_entry.text().to_string();
                let separator_arg = separator_entry.text().to_string();

                // Validate inputs
                if name.is_empty() || program.is_empty() || separator_arg.is_empty() {
                    this.add_toast(adw::Toast::new(&gettext("All fields are required")));
                    return;
                }

                // Create and save the terminal
                let terminal = supported_terminals::Terminal {
                    name,
                    program,
                    extra_args: vec![],
                    separator_arg,
                    read_only: false,
                };

                match this
                    .root_store()
                    .terminal_repository()
                    .save_terminal(terminal.clone())
                {
                    Ok(_) => {
                        // Show success toast
                        let toast = adw::Toast::new(&gettext("Custom terminal added successfully"));

                        if let Some(terminal_combo_row) =
                            this.imp().terminal_combo_row.borrow().as_ref()
                        {
                            terminal_combo_row.rebuild_terminals_list();
                            terminal_combo_row.set_selected_by_name(&terminal.name);
                        }

                        this.add_toast(toast);
                        custom_dialog.close();
                    }
                    Err(err) => {
                        error!(error = %err, "Failed to save terminal");
                        this.add_toast(adw::Toast::new(&gettext("Failed to save terminal")));
                    }
                }
            }
        ));

        custom_dialog.present(Some(self));
    }
}
