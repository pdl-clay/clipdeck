//! Renderização do popup: busca, lista e rodapé.

use cosmic::Element;
use cosmic::iced::{Alignment, Border, Length};
use cosmic::theme;
use cosmic::widget::space::vertical as vertical_space;
use cosmic::widget::{Column, container, divider, image, mouse_area, scrollable, text, text_input};

use crate::app::{Message, SCROLL_ID};
use crate::db::{Item, Kind};
use crate::util::{human_age, human_bytes};

/// Limite de caracteres mostrados na prévia de texto.
const PREVIEW_TEXT_CHARS: usize = 4000;

/// Conteúdo da janela do popup: lista à esquerda e prévia à direita.
pub fn popup<'a>(
    items: &'a [Item],
    thumbs: &'a [Option<image::Handle>],
    preview: Option<&'a image::Handle>,
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
        || {
            "1 clique prévia · 2 cliques cola · Enter cola · Ctrl+P fixa · Del remove · Esc fecha"
                .to_string()
        },
        ToString::to_string,
    );
    let footer = container(text::caption(hint))
        .padding([2, 8])
        .width(Length::Fill);

    // Painel de prévia: proporcional, para caber em popups estreitos.
    let pane_width = (width * 0.4).clamp(240.0, 420.0);
    let list_area = Column::with_children(vec![search.into(), list])
        .spacing(8)
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    let pane = container(preview_pane(items.get(focused), preview))
        .width(Length::Fixed(pane_width))
        .height(Length::Fill);
    let columns = cosmic::widget::row(vec![
        list_area,
        divider::vertical::light().into(),
        pane.into(),
    ])
    .spacing(12)
    .height(Length::Fill);

    let body = Column::with_children(vec![columns.into(), footer.into()])
        .spacing(8)
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill);

    container(body)
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .class(theme::Container::Custom(Box::new(|theme| {
            let cosmic = theme.cosmic();
            // `background(theme.transparent)` devolve a variante translúcida quando
            // o tema está com o vidro ligado (o compositor aplica o blur atrás).
            let container = cosmic.background(theme.transparent);
            container::Style {
                text_color: Some(container.on.into()),
                icon_color: Some(container.on.into()),
                background: Some(container.base.into()),
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

/// Prévia do item selecionado: imagem grande, ou o conteúdo em texto.
fn preview_pane<'a>(item: Option<&'a Item>, preview: Option<&'a image::Handle>) -> Element<'a, Message> {
    let Some(item) = item else {
        return container(text::caption("selecione um item"))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
    };

    let body: Element<'a, Message> = match (item.kind, preview) {
        (Kind::Image, Some(handle)) => container(
            image(handle.clone())
                .content_fit(cosmic::iced::ContentFit::Contain)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
        (Kind::Image, None) => text::caption("(imagem sem prévia)").into(),
        _ => {
            let text = item.text.trim();
            if text.is_empty() {
                text::caption("(sem texto)").into()
            } else {
                let shown: String = text.chars().take(PREVIEW_TEXT_CHARS).collect();
                scrollable(container(text::monotext(shown)).padding([0, 4]).width(Length::Fill))
                    .height(Length::Fill)
                    .into()
            }
        }
    };

    let caption = text::caption(format!(
        "{} · {} · {}",
        badge(item),
        human_bytes(item.bytes),
        human_age(item.created_at)
    ));

    Column::with_children(vec![body, caption.into()])
        .spacing(8)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Superfície de tela cheia transparente: o clique fora do painel fecha o popup
/// (somos nós que recebemos o clique, então o compositor não tira o foco no meio
/// do uso do mouse).
pub fn overlay<'a>(panel: Element<'a, Message>) -> Element<'a, Message> {
    let catcher = mouse_area(
        container(vertical_space())
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .on_press(Message::Hide);

    cosmic::iced::widget::stack(vec![
        catcher.into(),
        container(panel)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into(),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
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

    let highlight = theme::Button::Custom {
        active: Box::new(move |_focused, theme| row_style(theme, focused, false)),
        hovered: Box::new(move |_focused, theme| row_style(theme, focused, true)),
        pressed: Box::new(move |_focused, theme| row_style(theme, focused, true)),
        disabled: Box::new(|theme| row_style(theme, false, false)),
    };

    cosmic::widget::button::custom(cosmic::widget::row(content).spacing(8).align_y(Alignment::Center))
        .width(Length::Fill)
        .padding([6, 10])
        .on_press(Message::SelectIndex(index))
        .class(highlight)
        .into()
}

/// Fundo da linha: destacada quando é a linha focada; realce no hover.
fn row_style(theme: &cosmic::Theme, focused: bool, hovered: bool) -> cosmic::widget::button::Style {
    let cosmic = theme.cosmic();
    let container = cosmic.background(theme.transparent);
    cosmic::widget::button::Style {
        background: if focused || hovered {
            Some(container.component.hover.into())
        } else {
            None
        },
        border_radius: cosmic.radius_s().into(),
        border_width: if focused { 1.5 } else { 0.0 },
        border_color: cosmic.accent_color().into(),
        ..Default::default()
    }
}

fn badge(item: &Item) -> &'static str {
    match item.kind {
        Kind::Text => "texto",
        Kind::Html => "html",
        Kind::Image => "imagem",
        Kind::Files => "arquivo",
    }
}
