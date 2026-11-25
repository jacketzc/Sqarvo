//! 日期时间选择控件（参考 `gtk-datetime-picker` 的 GtkMenuButton + GtkPopover 结构）。
//!
//! 原生控件组合：`GtkEntry`（显示 / 手动输入）+ `GtkMenuButton`（日历图标）弹出
//! `GtkPopover`，面板里 `GtkCalendar` 选年月日、三个 `GtkSpinButton` 选时分秒，
//! 底部是「现在 / 应用」。值转换复用 [`crate::table_data`] 的纯函数，不引入新依赖。
//!
//! 本控件只产生文本，NULL / 省略 / 提交仍由调用方（单元格编辑器）处理。

use gtk::prelude::*;

use crate::l10n;
use crate::layout::{Gap, gap};
use crate::table_data::{
    DateTimeField, DateTimeParts, PickerValue, format_date_time, parse_picker_initial,
};

fn now_parts() -> DateTimeParts {
    gtk::glib::DateTime::now_local().map_or((1_970, 1, 1, 0, 0, 0, 0), |now| {
        (
            now.year(),
            now.month() as u32,
            now.day_of_month() as u32,
            now.hour() as u32,
            now.minute() as u32,
            now.second() as u32,
            0,
        )
    })
}

fn create_spin(max: i32, a11y_label: &str) -> gtk::SpinButton {
    let spin = gtk::SpinButton::with_range(0.0, f64::from(max), 1.0);
    spin.set_numeric(true);
    spin.set_wrap(true);
    spin.set_digits(0);
    spin.set_width_chars(2);
    spin.set_update_policy(gtk::SpinButtonUpdatePolicy::IfValid);
    spin.set_tooltip_text(Some(a11y_label));
    spin.update_property(&[gtk::accessible::Property::Label(a11y_label)]);
    spin.connect_output(|spin| {
        spin.set_text(&format!("{:02}", spin.value_as_int()));
        gtk::glib::Propagation::Stop
    });
    spin.set_text("00");
    spin
}

fn labeled_spin(caption: &str, spin: &gtk::SpinButton) -> gtk::Box {
    let block = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::Xs));
    block.set_hexpand(true);
    block.set_halign(gtk::Align::Center);
    let label = gtk::Label::new(Some(caption));
    label.add_css_class("dim-label");
    label.add_css_class("caption");
    block.append(&label);
    block.append(spin);
    block
}

/// 给已有的 `entry` 挂上日历按钮和选择面板，返回要放进编辑器的根控件
///（`GtkEntry` + `GtkMenuButton` 的 linked 组合）。调用方仍从同一个 `entry` 读文本。
///
/// `warning` 是调用方提供的覆盖提示：原值无法解析为日期时间时它会被点名显示。
pub fn attach(field: DateTimeField, entry: &gtk::Entry, warning: &gtk::Label) -> gtk::Box {
    let initial = entry.text().to_string();
    let value = match parse_picker_initial(field, &initial) {
        Ok(value) => value,
        Err(_) => {
            if !initial.trim().is_empty() {
                warning.set_text(&l10n::tf(
                    "data_page.editor_picker_unparseable",
                    &[("value", initial.clone())],
                ));
                warning.set_visible(true);
                entry.set_text(&format_date_time(field, now_parts(), None));
            }
            PickerValue {
                parts: now_parts(),
                offset_seconds: None,
            }
        }
    };
    let offset_seconds = value.offset_seconds;

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.add_css_class("linked");
    entry.set_hexpand(true);
    entry.set_width_chars(20);
    entry.set_placeholder_text(Some(l10n::t("data_page.editor_picker_placeholder")));

    let menu_button = gtk::MenuButton::builder()
        .icon_name("x-office-calendar-symbolic")
        .tooltip_text(l10n::t("data_page.editor_picker_button_tip"))
        .build();
    menu_button.set_tooltip_text(Some(l10n::t("data_page.editor_picker_button_tip")));
    menu_button.update_property(&[gtk::accessible::Property::Label(l10n::t(
        "data_page.editor_picker_button_tip",
    ))]);
    root.append(entry);
    root.append(&menu_button);

    let popover = gtk::Popover::new();
    popover.set_parent(&menu_button);
    popover.set_has_arrow(false);
    menu_button.set_popover(Some(&popover));

    let content = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::Sm));
    content.set_margin_top(gap(Gap::Sm));
    content.set_margin_bottom(gap(Gap::Sm));
    content.set_margin_start(gap(Gap::Sm));
    content.set_margin_end(gap(Gap::Sm));
    popover.set_child(Some(&content));

    let needs_date = matches!(
        field,
        DateTimeField::Date | DateTimeField::Datetime | DateTimeField::Timestamp
    );
    let needs_time = matches!(
        field,
        DateTimeField::Time | DateTimeField::Datetime | DateTimeField::Timestamp
    );

    let calendar = if needs_date {
        let calendar = gtk::Calendar::new();
        calendar.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "data_page.editor_picker_a11y_calendar",
        ))]);
        content.append(&calendar);
        Some(calendar)
    } else {
        None
    };

    if needs_date && needs_time {
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    }

    let (hour, minute, second) = if needs_time {
        let hour = create_spin(23, l10n::t("data_page.editor_picker_a11y_hour"));
        let minute = create_spin(59, l10n::t("data_page.editor_picker_a11y_minute"));
        let second = create_spin(59, l10n::t("data_page.editor_picker_a11y_second"));
        let row = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Sm));
        row.append(&labeled_spin(
            l10n::t("data_page.editor_picker_caption_hour"),
            &hour,
        ));
        row.append(&labeled_spin(
            l10n::t("data_page.editor_picker_caption_minute"),
            &minute,
        ));
        row.append(&labeled_spin(
            l10n::t("data_page.editor_picker_caption_second"),
            &second,
        ));
        content.append(&row);
        (Some(hour), Some(minute), Some(second))
    } else {
        (None, None, None)
    };

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Sm));
    let now_button = gtk::Button::with_label(l10n::t("data_page.editor_picker_now"));
    let apply_button = gtk::Button::with_label(l10n::t("data_page.editor_picker_apply"));
    apply_button.add_css_class("suggested-action");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    actions.append(&now_button);
    actions.append(&spacer);
    actions.append(&apply_button);
    content.append(&actions);

    // 面板每次打开都按输入框当前文本同步控件（手动改过文本也能接着用选择器）。
    {
        let entry = entry.clone();
        let calendar = calendar.clone();
        let hour = hour.clone();
        let minute = minute.clone();
        let second = second.clone();
        popover.connect_show(move |_| {
            let (year, month, day, h, m, s, _) = parse_picker_initial(field, &entry.text())
                .map(|value| value.parts)
                .unwrap_or_else(|_| now_parts());
            if let Some(calendar) = calendar.as_ref()
                && let Ok(date) =
                    gtk::glib::DateTime::from_local(year, month as i32, day as i32, 12, 0, 0.0)
            {
                calendar.set_date(Some(&date));
            }
            if let Some(hour) = hour.as_ref() {
                hour.set_value(f64::from(h));
            }
            if let Some(minute) = minute.as_ref() {
                minute.set_value(f64::from(m));
            }
            if let Some(second) = second.as_ref() {
                second.set_value(f64::from(s));
            }
        });
    }
    {
        let calendar = calendar.clone();
        let hour = hour.clone();
        let minute = minute.clone();
        let second = second.clone();
        now_button.connect_clicked(move |_| {
            let (year, month, day, h, m, s, _) = now_parts();
            if let Some(calendar) = calendar.as_ref()
                && let Ok(date) =
                    gtk::glib::DateTime::from_local(year, month as i32, day as i32, 12, 0, 0.0)
            {
                calendar.set_date(Some(&date));
            }
            if let Some(hour) = hour.as_ref() {
                hour.set_value(f64::from(h));
            }
            if let Some(minute) = minute.as_ref() {
                minute.set_value(f64::from(m));
            }
            if let Some(second) = second.as_ref() {
                second.set_value(f64::from(s));
            }
        });
    }
    {
        let entry = entry.clone();
        let calendar = calendar.clone();
        let hour = hour.clone();
        let minute = minute.clone();
        let second = second.clone();
        let weak_popover = popover.downgrade();
        apply_button.connect_clicked(move |_| {
            if let Some(spin) = &hour {
                spin.update();
            }
            if let Some(spin) = &minute {
                spin.update();
            }
            if let Some(spin) = &second {
                spin.update();
            }
            let (year, month, day) = calendar.as_ref().map_or((0, 0, 0), |calendar| {
                let date = calendar.date();
                (date.year(), date.month() as u32, date.day_of_month() as u32)
            });
            let parts = (
                year,
                month,
                day,
                hour.as_ref().map_or(0, |spin| spin.value_as_int() as u32),
                minute.as_ref().map_or(0, |spin| spin.value_as_int() as u32),
                second.as_ref().map_or(0, |spin| spin.value_as_int() as u32),
                0,
            );
            let offset = parse_picker_initial(field, &entry.text())
                .ok()
                .and_then(|value| value.offset_seconds)
                .or(offset_seconds);
            entry.set_text(&format_date_time(field, parts, offset));
            if let Some(popover) = weak_popover.upgrade() {
                popover.popdown();
            }
        });
    }
    // 手动输入格式不合法时给就地反馈；空文本（新行省略态）不算错。
    entry.connect_changed(move |entry| {
        let text = entry.text();
        if text.is_empty() || parse_picker_initial(field, &text).is_ok() {
            entry.remove_css_class("error");
        } else {
            entry.add_css_class("error");
        }
    });

    if crate::qa::enabled() {
        eprintln!(
            "[qa] 日期时间选择器：field={field:?} icon=1 calendar={} spins={} initial={:?} warning={:?}",
            usize::from(needs_date),
            usize::from(needs_time) * 3,
            entry.text(),
            warning.text()
        );
    }
    root
}
