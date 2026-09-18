//! Renderização do popup: busca, lista e rodapé.

use cosmic::Element;
use cosmic::iced::{Alignment, Border, Length};
use cosmic::theme;
use cosmic::widget::{Column, container, divider, image, mouse_area, scrollable, text, text_input};

use crate::app::{Message, SCROLL_ID};
use crate::db::{Item, Kind};
use crate::util::human_age;

/// Conteúdo da janela do popup.
pub fn popup<'a>(
    items: &'a [Item],
    thumbs: &'a [Option<image::Handle>],
    query: &'a str,
    focused: usize,
    status: Option<&'a str>,
    width: f32,
    height: f32,
) -> Element<'a, Message> {
    let search = text_input::search_input("buscar no histórico…", query)
        .on_input(Message::InputChanged)
        .on_submit(|_| Message::ActivateFocused { plain: false })
        .width(Length::Fill)
        .always_active();

    let list: Element<'a, Message> = if items.is_empty() {
        let message = if query.trim().is_empty() {
            "nada no histórico ainda"
        } else {
            "nada encontrado"
        };
        container(text::body(message))
            .padding(24)
            .center_x(Length::Fill)
            .into()
    } else {
        let rows: Vec<Element<'a, Message>> = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                row(
                    item,
                    thumbs.get(index).and_then(Option::as_ref),
                    index == focused,
                    index,
                )
            })
            .collect();
        scrollable(Column::with_children(rows).spacing(2).width(Length::Fill))
            .id(SCROLL_ID.clone())
            .height(Length::Fill)
            .into()
    };

    let hint = status.map_or_else(
        || "Enter cola · Shift+Enter texto puro · Ctrl+P fixa · Del remove · Esc fecha".to_string(),
        ToString::to_string,
    );
    let footer = container(text::caption(hint))
        .padding([2, 8])
        .width(Length::Fill);

    let body = Column::with_children(vec![
        search.into(),
        divider::horizontal::light().into(),
        list,
        footer.into(),
    ])
    .spacing(8)
    .padding(12)
    .width(Length::Fill)
    .height(Length::Fill);

    container(body)
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .class(theme::Container::Custom(Box::new(|theme| {
            let cosmic = theme.cosmic();
            container::Style {
                text_color: Some(cosmic.on_bg_color().into()),
                icon_color: Some(cosmic.on_bg_color().into()),
                background: Some(cosmic.bg_color().into()),
                border: Border {
                    radius: cosmic.radius_s().into(),
                    width: 1.0,
                    color: cosmic.bg_divider().into(),
                },
                ..Default::default()
            }
        })))
        .into()
}

fn row<'a>(
    item: &'a Item,
    thumb: Option<&'a image::Handle>,
    focused: bool,
    index: usize,
) -> Element<'a, Message> {
    let mut content: Vec<Element<'a, Message>> = Vec::with_capacity(5);
    content.push(
        text::caption(badge(item))
            .width(Length::Fixed(48.0))
            .into(),
    );
    if let Some(handle) = thumb {
        content.push(
            image(handle.clone())
                .width(Length::Fixed(28.0))
                .height(Length::Fixed(28.0))
                .into(),
        );
    }
    content.push(text::body(item.preview(90)).width(Length::Fill).into());
    if item.pinned {
        content.push(text::body("★").into());
    }
    content.push(
        text::caption(human_age(item.created_at))
            .width(Length::Shrink)
            .into(),
    );

    let highlight = theme::Container::Custom(Box::new(move |theme| {
        let cosmic = theme.cosmic();
        container::Style {
            background: focused.then(|| cosmic.bg_component_color().into()),
            border: Border {
                radius: cosmic.radius_s().into(),
                width: if focused { 1.5 } else { 0.0 },
                color: cosmic.accent_color().into(),
            },
            ..Default::default()
        }
    }));

    mouse_area(
        container(cosmic::widget::row(content).spacing(8).align_y(Alignment::Center))
            .padding([6, 10])
            .width(Length::Fill)
            .class(highlight),
    )
    .on_press(Message::ActivateIndex(index))
    .into()
}

fn badge(item: &Item) -> &'static str {
    match item.kind {
        Kind::Text => "texto",
        Kind::Html => "html",
        Kind::Image => "imagem",
        Kind::Files => "arquivo",
    }
}
