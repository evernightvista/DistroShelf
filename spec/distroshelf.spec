

%global forgeurl    https://github.com/evernightvista/DistroShelf
%global commit      6101296fc65b48a389dee47d60bf6d91f63e3f18
%global shortcommit %(c=%{commit}; echo ${c:0:7})
%global date        20260817

Name:       distroshelf
Version:    1.5.2
Release:    0.%{date}git%{shortcommit}%{?dist}
Summary:    GTK4 graphical manager for Distrobox containers

License:    GPL-3.0-or-later
URL:        %{forgeurl}
Source0:    https://github.com/ranfdev/DistroShelf/archive/refs/tags/v1.5.2.tar.gz


BuildRequires:  meson >= 1.0.0
BuildRequires:  ninja-build
BuildRequires:  rustc >= 1.85
BuildRequires:  cargo >= 1.85
BuildRequires:  gcc
BuildRequires:  pkgconfig(gtk4)          >= 4.16
BuildRequires:  pkgconfig(libadwaita-1)  >= 1.7
BuildRequires:  pkgconfig(vte-2.91-gtk4) >= 0.76
BuildRequires:  gettext
BuildRequires:  desktop-file-utils
BuildRequires:  appstream
BuildRequires:  /usr/bin/glib-compile-schemas
BuildRequires:  python3


Requires:       distrobox
Requires:       hicolor-icon-theme

Recommends:     (gnome-terminal or konsole or xfce4-terminal or tilix or alacritty)

%description
DistroShelf is a graphical application for managing Distrobox containers on
Linux. Built with GTK4 and libadwaita, it lets you create and manage
containers, view their status and details, install packages, manage exported
applications, open terminal sessions, and upgrade, clone or delete containers.

%prep
%autosetup -n DistroShelf-1.5.2 -p1

%build
%meson \
    %{?with_offline:-Doffline=true} \
    %{!?with_offline:-Doffline=false}
%meson_build

%install
%meson_install


rm -f %{buildroot}%{_datadir}/glib-2.0/schemas/gschemas.compiled
find %{buildroot}%{_datadir}/icons -type f -name 'icon-theme.cache' -delete 2>/dev/null || :
rm -f %{buildroot}%{_datadir}/applications/*.mime 2>/dev/null || :

%find_lang distroshelf

%check
%meson_test

%files -f distroshelf.lang
%license COPYING
%doc README.md
%{_bindir}/distroshelf
%{_datadir}/applications/com.ranfdev.DistroShelf.desktop
%{_datadir}/metainfo/com.ranfdev.DistroShelf.metainfo.xml
%{_datadir}/distroshelf/
%{_datadir}/glib-2.0/schemas/com.ranfdev.DistroShelf.gschema.xml
%{_datadir}/dbus-1/services/com.ranfdev.DistroShelf.service
%{_datadir}/icons/hicolor/scalable/apps/com.ranfdev.DistroShelf.svg
%{_datadir}/icons/hicolor/symbolic/apps/com.ranfdev.DistroShelf-symbolic.svg

%post
/usr/bin/update-desktop-database &>/dev/null || :
/usr/bin/gtk-update-icon-cache -f -t %{_datadir}/icons/hicolor &>/dev/null || :
/usr/bin/glib-compile-schemas %{_datadir}/glib-2.0/schemas &>/dev/null || :

%postun
/usr/bin/update-desktop-database &>/dev/null || :
/usr/bin/gtk-update-icon-cache -f -t %{_datadir}/icons/hicolor &>/dev/null || :
/usr/bin/glib-compile-schemas %{_datadir}/glib-2.0/schemas &>/dev/null || :

%changelog
* Sun Aug 17 2026 DistroShelf Maintainer <nobody@example.com> - 1.5.2-0.20260817git6101296
- Initial package: snapshot of main branch @ 6101296.
