---
name: gnome-sdk-docs
description: "Browse GNOME SDK documentation including GObject Introspection (.gir) files, D-Bus interfaces or icons that can be useful for GNOME apps development."
---

# Browsing GNOME SDK Documentation

This skill provides access to various GNOME SDK resources for development purposes.

## GObject Introspection Files

GObject Introspection (`.gir`) files are XML files describing the API of GNOME libraries. They are located at `/usr/share/gir-1.0/`.

The libraries used by this project and their corresponding files:

- `/usr/share/gir-1.0/Gtk-4.0.gir`
- `/usr/share/gir-1.0/Adw-1.gir`
- `/usr/share/gir-1.0/Gio-2.0.gir`
- `/usr/share/gir-1.0/GLib-2.0.gir`
- `/usr/share/gir-1.0/GObject-2.0.gir`
- `/usr/share/gir-1.0/Soup-3.0.gir`

These files are large XML documents. Use the `grep` tool to search for specific class names, method names, property names, or signal names rather than reading entire files. Examples:

- To find a class: use `grep` with pattern `<class name="Button"` in the relevant `.gir` file.
- To find a method: use `grep` with pattern `<method name="set_label"`.
- To find properties of a class: use `grep` with pattern `<property name=` near the class definition.
- To find signals: use `grep` with pattern `<glib:signal name=`.

## D-Bus Interfaces

D-Bus interface definitions are XML files describing D-Bus services and their methods, signals, and properties. They are located at `/usr/share/dbus-1/interfaces/`.

Use the `grep` tool to search for specific interface names, method names, or signal names. Example:

- To find an interface: use `grep` with pattern `<interface name="org.freedesktop.portal.Notification"` in the relevant interface file.

## Icons

GNOME icons are stored in `/usr/share/icons/`. This directory contains icon themes like Adwaita, with icons in various sizes and formats.

Use the `bash` tool with `ls` or the `ls` tool to browse available icons by theme and size. Examples:

- List themes: use `bash` with `ls /usr/share/icons/`
- List icons in a theme: use `bash` with `ls /usr/share/icons/Adwaita/`

Remember to use symbolic-icons for UI elements, unless showing the icon of a specific app