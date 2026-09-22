use serde::{Deserialize, Serialize};

/// 规则类型。首版只实现正则；`LocalModel` 仅保留枚举位以便后续扩展。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    #[default]
    Regex,
    LocalModel,
}

/// 命中后的附加校验，用于降低误报。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Validator {
    /// 银行卡 Luhn 校验
    Luhn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub name: String,
    /// 实体类型，会出现在占位符中，如 `EMAIL` -> `PG_EMAIL_xxxxxxxx`
    pub entity_type: String,
    #[serde(default)]
    pub kind: RuleKind,
    pub pattern: String,
    /// 只脱敏第 N 个捕获组（None 表示整段匹配）
    #[serde(default)]
    pub group: Option<usize>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub builtin: bool,
    /// 越大越优先；重叠时优先级高者胜出，其次取更长的匹配
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub validator: Option<Validator>,
    #[serde(default)]
    pub description: String,
}

fn default_true() -> bool {
    true
}

impl Rule {
    pub fn regex(id: &str, name: &str, entity: &str, pattern: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            entity_type: entity.to_string(),
            kind: RuleKind::Regex,
            pattern: pattern.to_string(),
            group: None,
            enabled: true,
            builtin: false,
            priority: 0,
            validator: None,
            description: String::new(),
        }
    }

    pub fn group(mut self, g: usize) -> Self {
        self.group = Some(g);
        self
    }

    pub fn priority(mut self, p: i32) -> Self {
        self.priority = p;
        self
    }

    pub fn builtin(mut self) -> Self {
        self.builtin = true;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn validator(mut self, v: Validator) -> Self {
        self.validator = Some(v);
        self
    }

    pub fn describe(mut self, d: &str) -> Self {
        self.description = d.to_string();
        self
    }
}

/// 内置规则集。用户可在 UI 中逐条启停，但不能删除。
pub fn builtin_rules() -> Vec<Rule> {
    vec![
        Rule::regex(
            "builtin.private_key",
            "私钥块",
            "PRIVATE_KEY",
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
        )
        .priority(100)
        .builtin()
        .describe("PEM 格式私钥"),
        Rule::regex(
            "builtin.jwt",
            "JWT",
            "JWT",
            r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
        )
        .priority(90)
        .builtin(),
        Rule::regex(
            "builtin.anthropic_key",
            "Anthropic API Key",
            "SECRET",
            r"\bsk-ant-[A-Za-z0-9_-]{20,}\b",
        )
        .priority(85)
        .builtin(),
        Rule::regex(
            "builtin.openai_key",
            "OpenAI API Key",
            "SECRET",
            r"\bsk-(?:proj-|svcacct-)?[A-Za-z0-9_-]{20,}\b",
        )
        .priority(80)
        .builtin(),
        Rule::regex(
            "builtin.aws_key",
            "AWS Access Key",
            "SECRET",
            r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
        )
        .priority(80)
        .builtin(),
        Rule::regex(
            "builtin.github_token",
            "GitHub Token",
            "SECRET",
            r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{60,})\b",
        )
        .priority(80)
        .builtin(),
        Rule::regex(
            "builtin.slack_token",
            "Slack Token",
            "SECRET",
            r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
        )
        .priority(80)
        .builtin(),
        Rule::regex(
            "builtin.google_api_key",
            "Google API Key",
            "SECRET",
            r"\bAIza[0-9A-Za-z_-]{35}\b",
        )
        .priority(80)
        .builtin(),
        Rule::regex(
            "builtin.bearer",
            "Bearer Token",
            "SECRET",
            r"(?i)\bBearer\s+([A-Za-z0-9\-._~+/]{20,}=*)",
        )
        .group(1)
        .priority(70)
        .builtin(),
        Rule::regex(
            "builtin.secret_assignment",
            "密钥赋值",
            "SECRET",
            r#"(?i)\b(?:api[_-]?key|secret[_-]?key|client[_-]?secret|password|passwd|access[_-]?token|auth[_-]?token|private[_-]?key)\b\s*[:=]\s*["']?([A-Za-z0-9_\-/+=.]{8,})["']?"#,
        )
        .group(1)
        .priority(60)
        .builtin()
        .describe("形如 API_KEY=xxxx / password: xxxx 的赋值"),
        Rule::regex(
            "builtin.email",
            "邮箱",
            "EMAIL",
            r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b",
        )
        .priority(50)
        .builtin(),
        Rule::regex(
            "builtin.id_card_cn",
            "中国大陆身份证",
            "ID_CARD",
            r"\b[1-9]\d{5}(?:19|20)\d{2}(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])\d{3}[0-9Xx]\b",
        )
        .priority(55)
        .builtin(),
        Rule::regex(
            "builtin.credit_card",
            "银行卡号",
            "CARD",
            r"\b(?:\d[ -]?){12,18}\d\b",
        )
        .priority(45)
        .validator(Validator::Luhn)
        .builtin(),
        Rule::regex(
            "builtin.phone_cn",
            "中国大陆手机号",
            "PHONE",
            r"\b1[3-9]\d{9}\b",
        )
        .priority(40)
        .builtin(),
        Rule::regex(
            "builtin.phone_intl",
            "国际电话",
            "PHONE",
            r"\+\d{1,3}[ -]?\(?\d{1,4}\)?[ -]?\d{3,4}[ -]?\d{3,4}\b",
        )
        .priority(40)
        .builtin(),
        Rule::regex(
            "builtin.ipv4",
            "IPv4 地址",
            "IP",
            r"\b(?:(?:25[0-5]|2[0-4]\d|1?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|1?\d?\d)\b",
        )
        .priority(30)
        .builtin()
        .disabled()
        .describe("代码中常见 127.0.0.1 等回环地址，默认关闭"),
    ]
}
