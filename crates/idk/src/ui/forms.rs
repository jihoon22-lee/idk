use crate::model::SourceSpec;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum FormKey {
    Connect,
    Rename(String),
    Environment(String),
    Reconnect(String),
    Repository(String),
    Terminal(String, Option<String>),
}

#[derive(Debug, Clone, Default)]
pub(super) struct TextInput {
    pub text: String,
    /// A UTF-8 byte boundary; movement/deletion never splits a Korean character.
    pub cursor: usize,
    pub dirty: bool,
}

impl TextInput {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            cursor: text.len(),
            text,
            dirty: false,
        }
    }

    pub fn insert(&mut self, text: &str) -> Result<(), &'static str> {
        if text.chars().any(char::is_control) {
            return Err(
                "Use a single line for this field; pasted control characters were not inserted.",
            );
        }
        if self.text.len() + text.len() > 8192 {
            return Err("This field is too long (maximum 8192 bytes).");
        }
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.dirty = true;
        Ok(())
    }

    pub fn key(&mut self, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control {
            match key.code {
                KeyCode::Char('a') => self.cursor = 0,
                KeyCode::Char('e') => self.cursor = self.text.len(),
                KeyCode::Char('u') => {
                    self.text.drain(..self.cursor);
                    self.cursor = 0;
                    self.dirty = true;
                }
                KeyCode::Char('k') => {
                    self.text.truncate(self.cursor);
                    self.dirty = true;
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Left => {
                self.cursor = self.text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(index, _)| index);
            }
            KeyCode::Right => {
                if let Some(character) = self.text[self.cursor..].chars().next() {
                    self.cursor += character.len_utf8();
                }
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Backspace if self.cursor > 0 => {
                let previous = self.text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(index, _)| index);
                self.text.drain(previous..self.cursor);
                self.cursor = previous;
                self.dirty = true;
            }
            KeyCode::Delete => {
                if let Some(character) = self.text[self.cursor..].chars().next() {
                    self.text
                        .drain(self.cursor..self.cursor + character.len_utf8());
                    self.dirty = true;
                }
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SUPER) =>
            {
                let _ = self.insert(&character.to_string());
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum FieldValue {
    Text(TextInput),
    Toggle(bool),
    Sources,
}

#[derive(Debug, Clone)]
pub(super) struct Field {
    pub label: String,
    pub help: String,
    pub value: FieldValue,
}

impl Field {
    pub fn text(label: &str, value: impl Into<String>, help: &str) -> Self {
        Self {
            label: label.into(),
            help: help.into(),
            value: FieldValue::Text(TextInput::new(value)),
        }
    }

    pub fn toggle(label: &str, value: bool, help: &str) -> Self {
        Self {
            label: label.into(),
            help: help.into(),
            value: FieldValue::Toggle(value),
        }
    }

    pub fn sources() -> Self {
        Self {
            label: "Initialization scripts".into(),
            help: "Enter opens the ordered script list; arguments have separate fields.".into(),
            value: FieldValue::Sources,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Form {
    pub key: FormKey,
    pub title: String,
    pub revision: u64,
    pub fields: Vec<Field>,
    pub selected: usize,
    pub sources: Vec<SourceSpec>,
}

impl Form {
    pub fn text(&self, index: usize) -> &str {
        match &self.fields[index].value {
            FieldValue::Text(input) => &input.text,
            _ => "",
        }
    }

    pub fn toggle(&self, index: usize) -> bool {
        matches!(self.fields[index].value, FieldValue::Toggle(true))
    }

    pub fn selected_is_sources(&self) -> bool {
        matches!(self.fields[self.selected].value, FieldValue::Sources)
    }

    pub fn edit(&mut self, key: KeyEvent) {
        let root_before = matches!(self.key, FormKey::Connect).then(|| self.text(1).to_owned());
        match key.code {
            KeyCode::Tab | KeyCode::Down => self.selected = (self.selected + 1) % self.fields.len(),
            KeyCode::BackTab | KeyCode::Up => {
                self.selected = (self.selected + self.fields.len() - 1) % self.fields.len();
            }
            KeyCode::Enter => match &mut self.fields[self.selected].value {
                FieldValue::Toggle(value) => *value = !*value,
                _ => self.selected = (self.selected + 1) % self.fields.len(),
            },
            _ => match &mut self.fields[self.selected].value {
                FieldValue::Text(input) => input.key(key),
                FieldValue::Toggle(value) if key.code == KeyCode::Char(' ') => *value = !*value,
                _ => {}
            },
        }
        if let Some(before) = root_before {
            self.follow_connect_root(&before);
        }
    }

    pub fn paste(&mut self, text: &str) -> Result<(), &'static str> {
        let root_before = matches!(self.key, FormKey::Connect).then(|| self.text(1).to_owned());
        if let FieldValue::Text(input) = &mut self.fields[self.selected].value {
            input.insert(text)?;
        }
        if let Some(before) = root_before {
            self.follow_connect_root(&before);
        }
        Ok(())
    }

    fn follow_connect_root(&mut self, before: &str) {
        let root = self.text(1).to_owned();
        if root != before {
            for index in [4, 7] {
                if let FieldValue::Text(input) = &mut self.fields[index].value {
                    if !input.dirty {
                        *input = TextInput::new(&root);
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct SourceList {
    pub parent: Form,
    pub sources: Vec<SourceSpec>,
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub(super) struct SourceEditor {
    pub parent: SourceList,
    pub index: Option<usize>,
    pub fields: Vec<Field>,
    pub selected: usize,
}

impl SourceEditor {
    pub fn new(parent: SourceList, index: Option<usize>) -> Self {
        let existing = index.and_then(|index| parent.sources.get(index));
        let mut fields = vec![Field::text(
            "Script path",
            existing.map_or(String::new(), |source| {
                source.path.to_string_lossy().into_owned()
            }),
            "Choose an existing .csh script. It is not executed while editing.",
        )];
        if let Some(source) = existing {
            for (index, argument) in source.args.iter().enumerate() {
                fields.push(Field::text(
                    &format!("Argument {}", index + 1),
                    argument,
                    "One literal argument. Spaces and quotes stay in this argument.",
                ));
            }
        }
        Self {
            parent,
            index,
            fields,
            selected: 0,
        }
    }

    pub fn value(&self) -> SourceSpec {
        let values: Vec<String> = self
            .fields
            .iter()
            .filter_map(|field| match &field.value {
                FieldValue::Text(input) => Some(input.text.clone()),
                _ => None,
            })
            .collect();
        SourceSpec {
            path: values[0].clone().into(),
            args: values[1..].to_vec(),
        }
    }

    pub fn edit(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::F(3) if self.fields.len() < 65 => {
                self.fields.push(Field::text(
                    &format!("Argument {}", self.fields.len()),
                    "",
                    "One literal argument. An empty field is an explicit empty argument.",
                ));
                self.selected = self.fields.len() - 1;
            }
            KeyCode::F(4) if self.selected > 0 => {
                self.fields.remove(self.selected);
                self.selected = self.selected.min(self.fields.len() - 1);
                for (index, field) in self.fields.iter_mut().enumerate().skip(1) {
                    field.label = format!("Argument {index}");
                }
            }
            KeyCode::Tab | KeyCode::Down | KeyCode::Enter => {
                self.selected = (self.selected + 1) % self.fields.len();
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.selected = (self.selected + self.fields.len() - 1) % self.fields.len();
            }
            _ => {
                if let FieldValue::Text(input) = &mut self.fields[self.selected].value {
                    input.key(key);
                }
            }
        }
    }
}

pub(super) fn visible_text(text: &str) -> String {
    text.chars()
        .flat_map(|character| {
            if character.is_control() {
                character.escape_default().collect::<Vec<_>>()
            } else {
                vec![character]
            }
        })
        .collect()
}
