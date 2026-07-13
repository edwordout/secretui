use super::*;

pub(super) fn draw(frame: &mut ratatui::Frame<'_>, app: &mut TuiApp) {
    app.sync_states();
    let area = centered_max_width(frame.area(), MAX_UI_WIDTH);
    let header_lines = wrap_text(&header(app), area.width.saturating_sub(2) as usize);
    let footer_lines = wrap_text(&footer(app), area.width as usize);
    let header_height = header_lines.len().saturating_add(2).min(u16::MAX as usize) as u16;
    let footer_height = footer_lines.len().min(u16::MAX as usize) as u16;
    let chrome_budget = area.height.saturating_sub(3);
    let footer_min = footer_height.min(1);
    let header_height = header_height.min(chrome_budget.saturating_sub(footer_min));
    let footer_height = footer_height.min(chrome_budget.saturating_sub(header_height));
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(0),
            Constraint::Length(footer_height),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(header_lines).block(Block::default().borders(Borders::ALL)),
        outer[0],
    );
    match app.mode {
        InputMode::Help => draw_help(frame, app, outer[1]),
        _ => match app.page {
            Page::Collections => draw_collections(frame, app, outer[1]),
            Page::Items => draw_items(frame, app, outer[1]),
            Page::Details => draw_details(frame, app, outer[1]),
            Page::Form => draw_form(frame, app, outer[1]),
        },
    }
    frame.render_widget(Paragraph::new(footer_lines), outer[2]);
}

pub(super) fn centered_max_width(area: Rect, max_width: u16) -> Rect {
    let width = area.width.min(max_width);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        width,
        ..area
    }
}

pub(super) fn wrap_text(text: &str, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    text.split('\n')
        .flat_map(|line| wrap_text_line(line, width))
        .map(Line::from)
        .collect()
}

pub(super) fn wrap_text_line(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }

    let indent = text
        .graphemes(true)
        .take_while(|grapheme| grapheme.chars().all(char::is_whitespace))
        .collect::<String>();
    let indent = if UnicodeWidthStr::width(indent.as_str()) < width {
        indent
    } else {
        String::new()
    };
    let mut lines = Vec::new();
    let mut line = indent.clone();
    let mut has_word = false;

    for word in text.split_whitespace() {
        let separator = usize::from(has_word);
        if has_word
            && UnicodeWidthStr::width(line.as_str()) + separator + UnicodeWidthStr::width(word)
                <= width
        {
            line.push(' ');
            line.push_str(word);
            continue;
        }
        if has_word {
            lines.push(line);
            line = indent.clone();
            has_word = false;
        }

        for grapheme in word.graphemes(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if has_word && UnicodeWidthStr::width(line.as_str()) + grapheme_width > width {
                lines.push(line);
                line = indent.clone();
                has_word = false;
            }
            if UnicodeWidthStr::width(line.as_str()) + grapheme_width <= width {
                line.push_str(grapheme);
                has_word = true;
            }
        }
    }

    if has_word || lines.is_empty() {
        lines.push(line);
    }
    lines
}

pub(super) fn wrapped_list_item(text: &str, width: usize) -> (ListItem<'static>, usize) {
    let width = width.max(1);
    let mut wrapped = wrap_text_line(text, width);
    if wrapped.len() > 2 {
        wrapped.truncate(2);
        let last = &mut wrapped[1];
        while UnicodeWidthStr::width(last.as_str()) + 1 > width {
            let Some((index, _)) = last.grapheme_indices(true).next_back() else {
                break;
            };
            last.truncate(index);
        }
        last.push('…');
    }
    let lines: Vec<_> = wrapped.into_iter().map(Line::from).collect();
    let height = lines.len().max(1);
    (ListItem::new(lines), height)
}

pub(super) fn scrollbar_position_for_rows(heights: &[usize], first_visible: usize) -> usize {
    heights.iter().take(first_visible).sum()
}

pub(super) fn draw_collections(
    frame: &mut ratatui::Frame<'_>,
    app: &mut TuiApp,
    area: ratatui::layout::Rect,
) {
    let action_lines = new_action_lines("New Collection", "press n", area.width.saturating_sub(2));
    let action_height =
        (action_lines.len().saturating_add(2) as u16).min(area.height.saturating_sub(3));
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(action_height), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(action_lines).block(Block::default().borders(Borders::ALL)),
        areas[0],
    );
    let row_width = areas[1].width.saturating_sub(4) as usize;
    let (rows, row_heights): (Vec<_>, Vec<_>) = app
        .collections
        .iter()
        .map(|collection| {
            wrapped_list_item(
                &format!(
                    "{}{}",
                    if collection.locked { "🔒 " } else { "" },
                    terminal_text::label(&collection.label)
                ),
                row_width,
            )
        })
        .unzip();
    frame.render_stateful_widget(
        List::new(rows)
            .highlight_symbol("› ")
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .block(Block::default().title("Collections").borders(Borders::ALL)),
        areas[1],
        &mut app.collection_state,
    );
    draw_scrollbar(
        frame,
        areas[1],
        row_heights.iter().sum(),
        scrollbar_position_for_rows(&row_heights, app.collection_state.offset()),
    );
}

pub(super) fn draw_items(
    frame: &mut ratatui::Frame<'_>,
    app: &mut TuiApp,
    area: ratatui::layout::Rect,
) {
    let action_lines = new_action_lines("New Item", "press n", area.width.saturating_sub(2));
    let action_height =
        (action_lines.len().saturating_add(2) as u16).min(area.height.saturating_sub(3));
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(action_height), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(action_lines).block(Block::default().borders(Borders::ALL)),
        areas[0],
    );
    let row_width = areas[1].width.saturating_sub(4) as usize;
    let (rows, row_heights): (Vec<_>, Vec<_>) = app
        .filtered_items()
        .iter()
        .map(|item| {
            wrapped_list_item(
                &format!(
                    "{}{}",
                    if item.locked { "🔒 " } else { "" },
                    terminal_text::label(&item.label)
                ),
                row_width,
            )
        })
        .unzip();
    let title = if app.filter.is_empty() {
        "Items".to_owned()
    } else {
        format!("Items /{}", terminal_text::attribute_value(&app.filter))
    };
    frame.render_stateful_widget(
        List::new(rows)
            .highlight_symbol("› ")
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .block(Block::default().title(title).borders(Borders::ALL)),
        areas[1],
        &mut app.item_state,
    );
    draw_scrollbar(
        frame,
        areas[1],
        row_heights.iter().sum(),
        scrollbar_position_for_rows(&row_heights, app.item_state.offset()),
    );
}

pub(super) fn draw_details(
    frame: &mut ratatui::Frame<'_>,
    app: &mut TuiApp,
    area: ratatui::layout::Rect,
) {
    let action_lines = action_button_lines(app, area.width.saturating_sub(2));
    let action_height =
        (action_lines.len().saturating_add(2) as u16).min(area.height.saturating_sub(3));
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(action_height), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(action_lines).block(Block::default().title("Actions").borders(Borders::ALL)),
        areas[0],
    );

    let (lines, revealed_row) =
        wrapped_detail_lines(app, areas[1].width.saturating_sub(2) as usize);
    let visible_rows = areas[1].height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(visible_rows.max(1));
    if app.reveal_scroll_pending {
        app.detail_scroll = revealed_row.unwrap_or(0).saturating_sub(2);
        app.reveal_scroll_pending = false;
    }
    app.detail_scroll = app.detail_scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.detail_scroll.min(u16::MAX as usize) as u16, 0))
            .block(Block::default().title("Details").borders(Borders::ALL)),
        areas[1],
    );
    draw_scrollbar(
        frame,
        areas[1],
        max_scroll + visible_rows,
        app.detail_scroll,
    );
}

pub(super) fn draw_form(frame: &mut ratatui::Frame<'_>, app: &mut TuiApp, area: Rect) {
    let Some(form) = &app.form else {
        return;
    };

    frame.render_widget(
        Block::default()
            .title(form_title(form))
            .borders(Borders::ALL),
        area,
    );
    let inner = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });

    let help = if form.kind == FormKind::DeleteItem {
        "Choose Cancel or Delete. Esc cancels."
    } else {
        "↑↓ move fields · ←/→ move cursor · Enter next/save · Tab also works · Esc cancel"
    };
    let mut help_lines = if form.kind == FormKind::DeleteItem {
        delete_confirmation_lines(
            &form.message,
            app.delete_snapshot.as_ref(),
            inner.width as usize,
        )
    } else {
        wrap_text(&terminal_text::error(&form.message), inner.width as usize)
    };
    help_lines.extend(wrap_text(help, inner.width as usize));
    let show_help = inner.height >= 8 && !help_lines.is_empty();
    let button_lines = form_button_lines(form, inner.width);
    let show_buttons = form.focus_buttons || inner.height >= 7;
    let button_height = if show_buttons {
        button_lines.len().min(u16::MAX as usize) as u16
    } else {
        0
    };
    let minimum_fields_height = if form.fields.is_empty() { 0 } else { 3 };
    let reserved_height = button_height
        .saturating_add(u16::from(show_buttons))
        .saturating_add(minimum_fields_height);
    let help_height = if show_help {
        (help_lines.len() as u16).min(inner.height.saturating_sub(reserved_height))
    } else {
        0
    };
    if show_help {
        frame.render_widget(
            Paragraph::new(help_lines),
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: help_height,
            },
        );
    }
    let button_area = Rect {
        x: inner.x,
        y: inner.bottom().saturating_sub(button_height),
        width: inner.width,
        height: button_height,
    };
    let fields_offset = help_height.saturating_add(u16::from(show_help));
    let fields_bottom_gap = u16::from(show_buttons && inner.height >= 7);
    let fields_area = Rect {
        x: inner.x,
        y: inner.y.saturating_add(fields_offset),
        width: inner.width,
        height: inner
            .height
            .saturating_sub(fields_offset)
            .saturating_sub(button_height)
            .saturating_sub(fields_bottom_gap),
    };

    let Some(form) = app.form.as_mut() else {
        return;
    };
    let field_heights = form_field_heights(form, fields_area.width);
    let field_offsets: Vec<_> = field_heights
        .iter()
        .scan(0usize, |offset, height| {
            let current = *offset;
            *offset = offset.saturating_add(*height).saturating_add(1);
            Some(current)
        })
        .collect();
    let content_height = field_heights
        .iter()
        .sum::<usize>()
        .saturating_add(field_heights.len());
    let max_scroll = content_height.saturating_sub(fields_area.height as usize);
    if !form.focus_buttons {
        let field_offset = field_offsets.get(form.selected_field).copied().unwrap_or(0);
        let field_height = field_heights.get(form.selected_field).copied().unwrap_or(3);
        keep_virtual_range_visible(
            &mut form.scroll,
            fields_area.height as usize,
            field_offset,
            field_height,
        );
    }
    form.scroll = form.scroll.min(max_scroll);

    for index in 0..form.fields.len() {
        let field_y = field_offsets[index];
        let field_height = field_heights[index];
        let Some(field_area) =
            virtual_rect_with_min(fields_area, field_y, field_height, form.scroll, 3)
        else {
            continue;
        };
        let focused = !form.focus_buttons && index == form.selected_field;
        if form.fields[index].kind == FormFieldKind::Attributes {
            draw_attribute_list_field(frame, form, focused, field_area);
        } else {
            draw_input_field(frame, &form.fields[index], focused, field_area);
        }
    }
    draw_content_scrollbar(frame, fields_area, content_height, form.scroll);

    if show_buttons {
        frame.render_widget(Paragraph::new(button_lines), button_area);
    }
}

pub(super) fn delete_confirmation_lines(
    message: &str,
    snapshot: Option<&DeleteSnapshot>,
    width: usize,
) -> Vec<Line<'static>> {
    let mut logical_lines = vec![terminal_text::error(message)];
    if let Some(snapshot) = snapshot {
        logical_lines.extend([
            format!("Item label: {}", terminal_text::label(&snapshot.item.label)),
            format!(
                "Collection: {}",
                terminal_text::label(&snapshot.collection.label)
            ),
            format!(
                "Collection path: {}",
                terminal_text::path(&snapshot.collection.path)
            ),
            format!("Item path: {}", terminal_text::path(&snapshot.item.path)),
            "WARNING: Metadata exports contain no secret values and cannot restore a deleted secret."
                .into(),
            "Attributes:".into(),
        ]);
        logical_lines.extend(
            snapshot
                .item
                .attributes
                .iter()
                .take(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT)
                .map(|(key, value)| {
                    format!(
                        "  {}={}",
                        terminal_text::attribute_key(key),
                        terminal_text::attribute_value(value)
                    )
                }),
        );
        let omitted = snapshot
            .item
            .attributes
            .len()
            .saturating_sub(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT);
        if omitted > 0 {
            logical_lines.push(format!("  … {omitted} attribute(s) not displayed"));
        } else if snapshot.item.attributes.is_empty() {
            logical_lines.push("  <none>".into());
        }
    }
    logical_lines
        .into_iter()
        .flat_map(|line| wrap_text(&line, width))
        .collect()
}

pub(super) fn form_title(form: &FormState) -> String {
    if form.dirty
        && matches!(
            form.kind,
            FormKind::NewCollection
                | FormKind::NewItem
                | FormKind::EditItem
                | FormKind::ReplaceSecret
        )
    {
        format!("{} (unsaved)", form.title())
    } else {
        form.title().into()
    }
}

pub(super) fn form_field_heights(form: &FormState, width: u16) -> Vec<usize> {
    form.fields
        .iter()
        .map(|field| {
            if field.kind != FormFieldKind::Attributes {
                return 3;
            }
            let row_width = width.saturating_sub(4) as usize;
            let rows_height: usize = form
                .attributes
                .iter()
                .take(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT)
                .map(|(key, value)| {
                    wrapped_list_item(
                        &format!(
                            "{} = {}",
                            terminal_text::attribute_key(key),
                            terminal_text::attribute_value(value)
                        ),
                        row_width,
                    )
                    .1
                })
                .sum::<usize>()
                .saturating_add(1)
                .saturating_add(usize::from(
                    form.attributes.len() > terminal_text::DISPLAYED_ATTRIBUTE_LIMIT,
                ));
            rows_height.saturating_add(2).clamp(5, 9)
        })
        .collect()
}

pub(super) fn draw_attribute_list_field(
    frame: &mut ratatui::Frame<'_>,
    form: &mut FormState,
    focused: bool,
    area: Rect,
) {
    let row_width = area.width.saturating_sub(4) as usize;
    let mut rows = Vec::new();
    let mut row_heights = Vec::new();
    for (key, value) in form
        .attributes
        .iter()
        .take(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT)
    {
        let (row, height) = wrapped_list_item(
            &format!(
                "{} = {}",
                terminal_text::attribute_key(key),
                terminal_text::attribute_value(value)
            ),
            row_width,
        );
        rows.push(row);
        row_heights.push(height);
    }
    rows.push(
        ListItem::new("+ Create new attribute").style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    );
    row_heights.push(1);

    let omitted = form
        .attributes
        .len()
        .saturating_sub(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT);
    if omitted > 0 {
        rows.push(ListItem::new(format!(
            "… {omitted} additional attribute(s) not displayed"
        )));
        row_heights.push(1);
    }

    form.selected_attribute = form.selected_attribute.min(displayed_attribute_count(form));
    let mut state = ListState::default().with_selected(Some(form.selected_attribute));
    frame.render_stateful_widget(
        List::new(rows)
            .highlight_symbol("› ")
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .block(
                Block::default()
                    .title("Attributes (Enter to edit)")
                    .borders(Borders::ALL)
                    .border_style(if focused {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    }),
            ),
        area,
        &mut state,
    );
    draw_scrollbar(
        frame,
        area,
        row_heights.iter().sum(),
        scrollbar_position_for_rows(&row_heights, state.offset()),
    );
}

pub(super) fn displayed_attribute_count(form: &FormState) -> usize {
    form.attributes
        .len()
        .min(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT)
}

pub(super) fn draw_input_field(
    frame: &mut ratatui::Frame<'_>,
    field: &FormField,
    focused: bool,
    area: Rect,
) {
    if area.height == 0 {
        return;
    }
    if area.height < 3 {
        let marker = if focused { "▶ " } else { "" };
        frame.render_widget(
            Paragraph::new(format!("{marker}{} …", terminal_text::label(&field.label))),
            area,
        );
        return;
    }

    let border_style = if field.error.is_some() {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let title = if let Some(error) = &field.error {
        format!(
            "{} ({})",
            terminal_text::label(&field.label),
            terminal_text::error(error)
        )
    } else if field.kind == FormFieldKind::Attributes {
        format!("{} (Enter to edit)", terminal_text::label(&field.label))
    } else {
        terminal_text::label(&field.label)
    };
    let inner_width = area.width.saturating_sub(2) as usize;
    let horizontal_scroll = if focused && inner_width > 0 {
        field.cursor.saturating_add(1).saturating_sub(inner_width)
    } else {
        0
    };
    frame.render_widget(
        Paragraph::new(terminal_text::terminal_safe(
            &field.visible_value(focused),
            form_field_render_limit(field),
        ))
        .scroll((0, horizontal_scroll.min(u16::MAX as usize) as u16))
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_style),
        ),
        area,
    );
}

pub(super) fn form_field_render_limit(field: &FormField) -> usize {
    match field.label.as_str() {
        "Key" => terminal_text::ATTRIBUTE_KEY_GRAPHEME_LIMIT,
        "Label" | "Alias" => terminal_text::LABEL_GRAPHEME_LIMIT,
        _ => terminal_text::ATTRIBUTE_VALUE_GRAPHEME_LIMIT,
    }
}

pub(super) fn button_row_lines(
    buttons: &[&str],
    selected_button: usize,
    focused: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let mut chunks = Vec::new();
    for (index, button) in buttons.iter().enumerate() {
        let selected = focused && index == selected_button;
        let destructive = matches!(*button, "Delete" | "Remove" | "Discard");
        let style = if selected && destructive {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else if destructive {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut chunk = Vec::new();
        if destructive && index > 0 {
            chunk.push(Span::raw("    "));
        }
        chunk.extend([Span::styled(format!(" {button} "), style), Span::raw("  ")]);
        chunks.push(chunk);
    }
    wrap_styled_chunks(vec![Span::raw("  ")], chunks, width as usize)
}

pub(super) fn form_button_lines(form: &FormState, width: u16) -> Vec<Line<'static>> {
    button_row_lines(
        form.buttons(),
        form.selected_button,
        form.focus_buttons,
        width,
    )
}

pub(super) fn keep_virtual_range_visible(
    scroll: &mut usize,
    viewport_height: usize,
    target_y: usize,
    target_height: usize,
) {
    if viewport_height == 0 || target_y < *scroll {
        *scroll = target_y;
        return;
    }
    let target_bottom = target_y.saturating_add(target_height);
    let viewport_bottom = scroll.saturating_add(viewport_height);
    if target_bottom > viewport_bottom {
        *scroll = target_bottom.saturating_sub(viewport_height);
    }
}

pub(super) fn virtual_rect_with_min(
    container: Rect,
    y: usize,
    height: usize,
    scroll: usize,
    min_visible_height: u16,
) -> Option<Rect> {
    let top = y as isize - scroll as isize;
    let bottom = top + height as isize;
    let visible_top = top.max(0);
    let visible_bottom = bottom.min(container.height as isize);
    if visible_bottom <= visible_top {
        return None;
    }
    let visible_height = (visible_bottom - visible_top) as u16;
    if visible_height < min_visible_height {
        return None;
    }
    Some(Rect {
        x: container.x,
        y: container.y.saturating_add(visible_top as u16),
        width: container.width,
        height: visible_height,
    })
}

pub(super) fn draw_content_scrollbar(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    content_len: usize,
    position: usize,
) {
    let visible_len = area.height as usize;
    if content_len <= visible_len || visible_len == 0 {
        return;
    }
    let mut state = ScrollbarState::new(content_len.saturating_sub(visible_len)).position(position);
    frame.render_stateful_widget(
        Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓")),
        area,
        &mut state,
    );
}

pub(super) fn draw_scrollbar(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    content_len: usize,
    position: usize,
) {
    let visible_len = area.height.saturating_sub(2) as usize;
    if content_len <= visible_len || visible_len == 0 {
        return;
    }
    let mut state = ScrollbarState::new(content_len.saturating_sub(visible_len)).position(position);
    frame.render_stateful_widget(
        Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓")),
        area.inner(ratatui::layout::Margin {
            horizontal: 0,
            vertical: 1,
        }),
        &mut state,
    );
}

pub(super) fn draw_help(
    frame: &mut ratatui::Frame<'_>,
    app: &mut TuiApp,
    area: ratatui::layout::Rect,
) {
    let lines: Vec<_> = help_lines()
        .into_iter()
        .flat_map(|line| {
            let text = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            wrap_text(&text, area.width.saturating_sub(2) as usize)
        })
        .collect();
    let visible_rows = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(visible_rows.max(1));
    app.help_scroll = app.help_scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.help_scroll.min(u16::MAX as usize) as u16, 0))
            .block(Block::default().title("Help").borders(Borders::ALL)),
        area,
    );
    draw_scrollbar(frame, area, max_scroll + visible_rows, app.help_scroll);
}

pub(super) fn header(app: &TuiApp) -> String {
    let page = match app.page {
        Page::Collections => "Collections".into(),
        Page::Items => "Items".into(),
        Page::Details => "Details".into(),
        Page::Form => app
            .form
            .as_ref()
            .map(form_title)
            .unwrap_or_else(|| "Form".into()),
    };
    let collection = app
        .selected_collection()
        .map(|collection| terminal_text::label(&collection.label))
        .unwrap_or_else(|| "none".into());
    let item = app
        .selected_item()
        .map(|item| terminal_text::label(&item.label))
        .unwrap_or_else(|| "none".into());
    let mut header = format!("secretui  /  {page}\ncollection: {collection}  |  item: {item}");
    if !app.message.is_empty() {
        write!(header, "\nstatus: {}", terminal_text::error(&app.message))
            .expect("write to string");
    }
    header
}

pub(super) fn footer(app: &TuiApp) -> String {
    match app.mode {
        InputMode::Search => format!(
            "Search: {}  ·  Enter/Esc finish  ·  Backspace edit",
            terminal_text::attribute_value(&app.filter)
        ),
        InputMode::Help => "↑↓ scroll  ·  Esc / ? close help".into(),
        InputMode::Normal => match app.page {
            Page::Collections => {
                "↑↓ choose collection  ·  Enter/→ items  ·  n New Collection  ·  / search  ·  ? help  ·  q quit".into()
            }
            Page::Items => {
                "↑↓ choose item  ·  Enter/→ details  ·  ←/Esc collections  ·  n New Item  ·  / search  ·  ? help  ·  q quit".into()
            }
            Page::Details => {
                "↑↓ scroll  ·  ←/→ choose button  ·  Enter activate  ·  r reveal  ·  Esc back  ·  ? help  ·  q quit".into()
            }
            Page::Form => {
                "↑↓ move/scroll  ·  ←/→ cursor/buttons  ·  Enter next/activate  ·  Esc cancel".into()
            }
        },
    }
}

pub(super) fn help_lines() -> Vec<Line<'static>> {
    vec![
        Line::from("Pages"),
        Line::from("  Collections → Items → Details"),
        Line::from(""),
        Line::from("Navigation"),
        Line::from("  ↑/↓ or j/k: move"),
        Line::from("  Enter/→/l/Tab: next page"),
        Line::from("  Esc/h: previous page or close overlay"),
        Line::from("  Details: ↑/↓ scroll, ←/→ choose button, Enter activates"),
        Line::from("  Home/End/PgUp/PgDn: jump"),
        Line::from("  Forms: ↑↓ move/scroll, ←→ cursor/buttons, Enter next/activate"),
        Line::from(""),
        Line::from("Actions"),
        Line::from("  Collections: n New Collection"),
        Line::from("  Items: n New Item"),
        Line::from("  Details: r explicitly reveals the selected secret"),
        Line::from(
            "  Details buttons: Back/reveal/copy text/copy Base64/copy hex/edit metadata/replace secret/delete/lock",
        ),
        Line::from(
            "  Reveal/Copy/Edit/Replace/Delete may temporarily unlock the needed collection or item; SecretUI restores its original lock state.",
        ),
        Line::from("  / search  ? help  q quit"),
        Line::from(""),
        Line::from("Secrets stay hidden unless explicitly revealed or copied."),
    ]
}

pub(super) fn detail_lines(app: &TuiApp) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(""),
        Line::from(terminal_text::error(&app.message)),
        Line::from(
            "Explicit Reveal/Copy/Edit/Replace/Delete authorizes provider prompts and any temporary unlock of the needed collection or item; SecretUI restores its original lock state.",
        ),
        Line::from(""),
    ];
    if let Some(collection) = app.selected_collection() {
        lines.push(Line::from("Collection"));
        lines.push(Line::from(format!(
            "  label: {}",
            terminal_text::label(&collection.label)
        )));
        lines.push(Line::from(format!(
            "  path: {}",
            terminal_text::path(&collection.path)
        )));
        lines.push(Line::from(""));
    }
    if let Some(item) = app.selected_item() {
        lines.push(Line::from("Item"));
        lines.push(Line::from(format!(
            "  label: {}",
            terminal_text::label(&item.label)
        )));
        lines.push(Line::from(format!(
            "  path: {}",
            terminal_text::path(&item.path)
        )));
        lines.push(Line::from(format!("  locked: {}", item.locked)));
        lines.push(Line::from(""));
        lines.push(Line::from("Attributes"));
        for (key, value) in item
            .attributes
            .iter()
            .take(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT)
        {
            lines.push(Line::from(format!(
                "  {}={}",
                terminal_text::attribute_key(key),
                terminal_text::attribute_value(value)
            )));
        }
        let omitted = item
            .attributes
            .len()
            .saturating_sub(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT);
        if omitted > 0 {
            lines.push(Line::from(format!(
                "  … {omitted} attribute(s) not displayed"
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from("Secret"));
        lines.push(Line::from("  <hidden>"));
        let metadata = app
            .secret_metadata
            .as_ref()
            .filter(|metadata| metadata.item_path == item.path);
        lines.push(Line::from(format!(
            "  content type: {}",
            metadata
                .map(|metadata| terminal_text::attribute_value(&metadata.content_type))
                .as_deref()
                .unwrap_or("unavailable until Reveal or Copy")
        )));
        lines.push(Line::from(format!(
            "  size: {}",
            metadata
                .map(|metadata| format!("{} bytes", metadata.size))
                .unwrap_or_else(|| "unavailable until Reveal or Copy".into())
        )));
        lines.push(Line::from(format!(
            "  encoding: {}",
            metadata
                .map(|metadata| metadata.encoding.label())
                .unwrap_or("unavailable until Reveal or Copy")
        )));
        if let Some(reveal) = &app.reveal {
            if reveal.item_path == item.path {
                lines.push(Line::from("  preview:"));
                lines.extend(
                    secret_preview(&reveal.value)
                        .into_iter()
                        .map(|line| Line::from(format!("    {line}"))),
                );
            }
        }
        lines.push(Line::from(""));
    }
    lines
}

pub(super) fn wrapped_detail_lines(
    app: &TuiApp,
    width: usize,
) -> (Vec<Line<'static>>, Option<usize>) {
    let mut wrapped = Vec::new();
    let mut revealed_row = None;
    for line in detail_lines(app) {
        let is_revealed = line
            .spans
            .iter()
            .any(|span| span.content.contains("preview:"));
        if is_revealed {
            revealed_row = Some(wrapped.len());
        }
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        wrapped.extend(wrap_text(&text, width));
    }
    (wrapped, revealed_row)
}

pub(super) fn action_button_lines(app: &TuiApp, width: u16) -> Vec<Line<'static>> {
    let mut chunks = Vec::new();
    for (index, action) in DETAIL_ACTIONS.iter().enumerate() {
        let selected = index == app.selected_action;
        let label = match action {
            DetailAction::Reveal => "Reveal",
            DetailAction::CopyText => "Copy Text",
            DetailAction::CopyBase64 => "Copy Base64",
            DetailAction::CopyHex => "Copy Hex",
            DetailAction::Edit => "Edit Metadata",
            DetailAction::ReplaceSecret => "Replace Secret",
            DetailAction::Delete => "Delete",
            DetailAction::LockUnlock => {
                if app
                    .selected_collection()
                    .is_some_and(|collection| collection.locked)
                {
                    "Unlock"
                } else {
                    "Lock"
                }
            }
            DetailAction::Back => "Back",
        };
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        chunks.push(vec![
            Span::styled(format!(" {label} "), style),
            Span::raw(" "),
        ]);
    }
    wrap_styled_chunks(vec![Span::raw("Actions: ")], chunks, width as usize)
}

pub(super) fn new_action_lines(button: &str, hint: &str, width: u16) -> Vec<Line<'static>> {
    let button_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    wrap_styled_chunks(
        vec![Span::raw("Actions: ")],
        vec![
            vec![Span::styled(format!(" {button} "), button_style)],
            vec![Span::raw(format!("  {hint}"))],
        ],
        width as usize,
    )
}

pub(super) fn wrap_styled_chunks(
    prefix: Vec<Span<'static>>,
    chunks: Vec<Vec<Span<'static>>>,
    width: usize,
) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut spans = prefix;
    let mut line_width: usize = spans.iter().map(Span::width).sum();
    for chunk in chunks {
        let chunk_width: usize = chunk.iter().map(Span::width).sum();
        if !spans.is_empty() && line_width.saturating_add(chunk_width) > width {
            lines.push(Line::from(spans));
            spans = Vec::new();
            line_width = 0;
        }
        line_width = line_width.saturating_add(chunk_width);
        spans.extend(chunk);
    }
    if !spans.is_empty() || lines.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}
