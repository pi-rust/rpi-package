use serde_json::{Value, json};

#[derive(Debug, Clone, Default)]
pub struct MessageCardBuilder {
    template: Option<String>,
    title: Option<String>,
    elements: Vec<Value>,
}

impl MessageCardBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn template(mut self, template: impl Into<String>) -> Self {
        self.template = Some(template.into());
        self
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn text(mut self, content: impl Into<String>) -> Self {
        self.elements.push(json!({
            "tag": "markdown",
            "content": content.into()
        }));
        self
    }

    pub fn button(mut self, button: ButtonBuilder) -> Self {
        self.elements.push(button.build());
        self
    }

    pub fn image(mut self, image: ImageBuilder) -> Self {
        self.elements.push(image.build());
        self
    }

    pub fn form(mut self, form: FormBuilder) -> Self {
        self.elements.push(form.build());
        self
    }

    pub fn element(mut self, element: Value) -> Self {
        self.elements.push(element);
        self
    }

    pub fn build(self) -> Value {
        json!({
            "config": {
                "wide_screen_mode": true
            },
            "header": {
                "template": self.template.unwrap_or_else(|| "blue".to_string()),
                "title": {
                    "tag": "plain_text",
                    "content": self.title.unwrap_or_else(|| "Message".to_string())
                }
            },
            "elements": self.elements
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ButtonBuilder {
    text: Option<String>,
    value: Option<Value>,
    kind: Option<String>,
}

impl ButtonBuilder {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            ..Default::default()
        }
    }

    pub fn value(mut self, value: Value) -> Self {
        self.value = Some(value);
        self
    }

    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    pub fn build(self) -> Value {
        json!({
            "tag": "button",
            "type": self.kind.unwrap_or_else(|| "default".to_string()),
            "text": {
                "tag": "plain_text",
                "content": self.text.unwrap_or_else(|| "Button".to_string())
            },
            "value": self.value.unwrap_or_else(|| json!({}))
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImageBuilder {
    img_key: Option<String>,
    alt: Option<String>,
}

impl ImageBuilder {
    pub fn new(img_key: impl Into<String>) -> Self {
        Self {
            img_key: Some(img_key.into()),
            ..Default::default()
        }
    }

    pub fn alt(mut self, alt: impl Into<String>) -> Self {
        self.alt = Some(alt.into());
        self
    }

    pub fn build(self) -> Value {
        json!({
            "tag": "img",
            "img_key": self.img_key.unwrap_or_default(),
            "alt": {
                "tag": "plain_text",
                "content": self.alt.unwrap_or_else(|| "image".to_string())
            }
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct FormBuilder {
    name: Option<String>,
    label: Option<String>,
}

impl FormBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            ..Default::default()
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn build(self) -> Value {
        json!({
            "tag": "input",
            "name": self.name.unwrap_or_else(|| "field".to_string()),
            "placeholder": {
                "tag": "plain_text",
                "content": self.label.unwrap_or_else(|| "Input".to_string())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_card_with_template_and_elements() {
        let card = MessageCardBuilder::new()
            .template("wathet")
            .title("Approval update")
            .text("A request is waiting for your decision.")
            .button(ButtonBuilder::new("Approve").kind("primary"))
            .image(ImageBuilder::new("img_v3_xxx").alt("preview"))
            .form(FormBuilder::new("comment").label("Add a comment"))
            .build();

        assert_eq!(card["header"]["template"], "wathet");
        assert_eq!(card["header"]["title"]["content"], "Approval update");
        assert!(card["elements"].as_array().is_some());
    }
}
