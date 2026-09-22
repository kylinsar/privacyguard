import { useEffect, useMemo, useState } from "react";
import {
  Alert, Button, Card, Col, Input, Popconfirm, Radio, Row, Select, Space, Switch, Table, Tag, Typography,
  App as AntApp,
} from "antd";
import { PlusOutlined, DeleteOutlined, ExperimentOutlined, CheckCircleFilled } from "@ant-design/icons";
import { api, type IdentityField, type Persona, type PersonaView, type RuleTestResult, type SubstitutionStyle } from "../api";

const ENTITY_OPTIONS = [
  { value: "NAME", label: "姓名" },
  { value: "PHONE", label: "手机 / 电话" },
  { value: "EMAIL", label: "邮箱" },
  { value: "ORG", label: "公司 / 组织" },
  { value: "ADDRESS", label: "地址" },
  { value: "ID_CARD", label: "身份证" },
  { value: "ACCOUNT", label: "账号 / 用户名" },
  { value: "PROJECT", label: "项目 / 产品名" },
  { value: "CUSTOM", label: "自定义" },
];

const PRESET_FIELDS: Omit<IdentityField, "id">[] = [
  { label: "姓名", entity_type: "NAME", real_values: [], alias: "", case_insensitive: true, enabled: true },
  { label: "手机号", entity_type: "PHONE", real_values: [], alias: "", case_insensitive: false, enabled: true },
  { label: "邮箱", entity_type: "EMAIL", real_values: [], alias: "", case_insensitive: true, enabled: true },
  { label: "公司", entity_type: "ORG", real_values: [], alias: "", case_insensitive: true, enabled: true },
];

function newId() {
  return Math.random().toString(36).slice(2, 10);
}

function newPersona(): Persona {
  return {
    id: "persona-" + newId(),
    name: "新身份",
    description: "",
    fields: PRESET_FIELDS.map((f) => ({ ...f, id: newId() })),
    updated_at: "",
  };
}

export default function Personas() {
  const [view, setView] = useState<PersonaView | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [draft, setDraft] = useState<Persona | null>(null);
  const [dirty, setDirty] = useState(false);
  const [previewText, setPreviewText] = useState("请把这份材料发给我，我是【姓名】，电话【手机】，邮箱【邮箱】，我在【公司】工作。另外抄送同事 peer@example.com。");
  const [preview, setPreview] = useState<RuleTestResult | null>(null);
  const [busy, setBusy] = useState(false);
  const { message } = AntApp.useApp();

  const load = async () => {
    const v = await api.listPersonas();
    setView(v);
    const pick = selected && v.personas.some((p) => p.id === selected) ? selected : v.active_persona ?? v.personas[0]?.id ?? null;
    setSelected(pick);
    const p = v.personas.find((x) => x.id === pick) ?? null;
    setDraft(p ? structuredClone(p) : null);
    setDirty(false);
  };
  useEffect(() => {
    load().catch((e) => message.error(String(e)));
  }, []);

  const select = (id: string) => {
    if (!view) return;
    setSelected(id);
    const p = view.personas.find((x) => x.id === id) ?? null;
    setDraft(p ? structuredClone(p) : null);
    setDirty(false);
    setPreview(null);
  };

  const create = () => {
    const p = newPersona();
    setSelected(p.id);
    setDraft(p);
    setDirty(true);
    setPreview(null);
  };

  const update = (patch: Partial<Persona>) => {
    if (!draft) return;
    setDraft({ ...draft, ...patch });
    setDirty(true);
  };

  const updateField = (id: string, patch: Partial<IdentityField>) => {
    if (!draft) return;
    update({ fields: draft.fields.map((f) => (f.id === id ? { ...f, ...patch } : f)) });
  };

  const addField = () => {
    if (!draft) return;
    update({
      fields: [...draft.fields, { id: newId(), label: "", entity_type: "CUSTOM", real_values: [], alias: "", case_insensitive: false, enabled: true }],
    });
  };

  const save = async () => {
    if (!draft) return;
    if (!draft.name.trim()) {
      message.warning("请填写身份名称");
      return;
    }
    setBusy(true);
    try {
      const v = await api.upsertPersona(draft);
      setView(v);
      setDirty(false);
      const p = v.personas.find((x) => x.id === draft.id);
      if (p) setDraft(structuredClone(p));
      message.success("已保存");
    } catch (e) {
      message.error(String(e), 6);
    } finally {
      setBusy(false);
    }
  };

  const remove = async (id: string) => {
    setBusy(true);
    try {
      const v = await api.deletePersona(id);
      setView(v);
      setSelected(null);
      setDraft(null);
      await load();
    } catch (e) {
      message.error(String(e), 6);
    } finally {
      setBusy(false);
    }
  };

  const setOptions = async (active: string | null, style: SubstitutionStyle) => {
    setBusy(true);
    try {
      setView(await api.setPersonaOptions(active, style));
      message.success(active ? "已启用该身份" : "已关闭隐身身份");
    } catch (e) {
      message.error(String(e), 6);
    } finally {
      setBusy(false);
    }
  };

  const runPreview = async () => {
    if (!draft) return;
    setBusy(true);
    try {
      // 把占位符【姓名】等替换成草稿里的真实值，方便直接预览
      let text = previewText;
      for (const f of draft.fields) {
        const real = f.real_values[0];
        if (real && f.label) text = text.split(`【${f.label}】`).join(real);
      }
      setPreview(await api.previewPersona(text, draft));
    } catch (e) {
      message.error(String(e), 6);
    } finally {
      setBusy(false);
    }
  };

  const usable = useMemo(
    () => draft?.fields.filter((f) => f.enabled && f.alias.trim() && f.real_values.some((v) => v.trim() && v.trim() !== f.alias.trim())).length ?? 0,
    [draft],
  );
  const isActive = !!view && !!draft && view.active_persona === draft.id;

  return (
    <div>
      <h1 className="pg-page-title">隐身身份</h1>
      <Alert
        type="info"
        showIcon
        style={{ marginBottom: 12 }}
        message="把你的真实信息替换成一套自洽的假身份"
        description="正则规则只能识别有格式的信息（手机、邮箱、卡号）；姓名、公司、地址这类信息模型无法识别，但你自己知道。在这里填入真实写法和想让模型看到的隐身值，请求发出前会精确替换，回复回来时再换回真实值。真实值只保存在本机数据库，不会发往任何服务器。"
      />
      <Row gutter={16}>
        <Col span={7}>
          <Card
            title="身份列表"
            size="small"
            extra={<Button size="small" icon={<PlusOutlined />} onClick={create}>新建</Button>}
          >
            {view?.personas.length === 0 && !draft && <Typography.Text type="secondary">还没有身份，点「新建」开始。</Typography.Text>}
            <Space direction="vertical" style={{ width: "100%" }}>
              {(view?.personas ?? []).concat(draft && !view?.personas.some((p) => p.id === draft.id) ? [draft] : []).map((p) => (
                <div
                  key={p.id}
                  onClick={() => select(p.id)}
                  className={"pg-list-item" + (p.id === selected ? " active" : "")}
                  style={{ padding: "8px 10px", borderRadius: 6, cursor: "pointer", border: "1px solid " + (p.id === selected ? "#1e40af" : "#e5e7eb") }}
                >
                  <Space>
                    {view?.active_persona === p.id && <CheckCircleFilled style={{ color: "#16a34a" }} />}
                    <span>{p.name || "(未命名)"}</span>
                    <Tag>{p.fields.length} 字段</Tag>
                  </Space>
                </div>
              ))}
            </Space>
          </Card>

          <Card title="替换风格" size="small" style={{ marginTop: 12 }}>
            <Radio.Group
              value={view?.substitution_style ?? "placeholder"}
              disabled={busy || !view}
              onChange={(e) => view && setOptions(view.active_persona, e.target.value as SubstitutionStyle)}
            >
              <Space direction="vertical">
                <Radio value="placeholder">
                  占位符 <Typography.Text type="secondary" style={{ fontSize: 12 }}>PG_EMAIL_1a2b3c4d，模型一眼能看出是占位</Typography.Text>
                </Radio>
                <Radio value="synthetic">
                  拟真假值 <Typography.Text type="secondary" style={{ fontSize: 12 }}>同类型假邮箱 / 假号码 / 假卡号，看起来像真的</Typography.Text>
                </Radio>
              </Space>
            </Radio.Group>
            <Typography.Paragraph type="secondary" style={{ fontSize: 12, marginTop: 8, marginBottom: 0 }}>
              作用于未被身份字段覆盖的敏感值（如对话里出现的其他人邮箱）。密钥、JWT 等始终使用占位符。
            </Typography.Paragraph>
          </Card>
        </Col>

        <Col span={17}>
          {!draft ? (
            <Card><Typography.Text type="secondary">选择或新建一个身份。</Typography.Text></Card>
          ) : (
            <Card
              size="small"
              title={
                <Space>
                  <Input
                    value={draft.name}
                    onChange={(e) => update({ name: e.target.value })}
                    placeholder="身份名称，如「工作身份」"
                    style={{ width: 220 }}
                  />
                  {isActive ? <Tag color="green">当前启用</Tag> : <Tag>未启用</Tag>}
                  {dirty && <Tag color="orange">未保存</Tag>}
                </Space>
              }
              extra={
                <Space>
                  {isActive ? (
                    <Button disabled={busy} onClick={() => setOptions(null, view!.substitution_style)}>停用</Button>
                  ) : (
                    <Button
                      disabled={busy || dirty || !view?.personas.some((p) => p.id === draft.id)}
                      onClick={() => setOptions(draft.id, view!.substitution_style)}
                      title={dirty ? "请先保存" : undefined}
                    >
                      启用此身份
                    </Button>
                  )}
                  <Popconfirm title="删除该身份？" onConfirm={() => remove(draft.id)}>
                    <Button danger icon={<DeleteOutlined />} disabled={busy} />
                  </Popconfirm>
                  <Button type="primary" loading={busy} disabled={!dirty} onClick={save}>保存</Button>
                </Space>
              }
            >
              <Input
                value={draft.description}
                onChange={(e) => update({ description: e.target.value })}
                placeholder="备注（可选）"
                style={{ marginBottom: 12 }}
              />
              <Table
                size="small"
                rowKey="id"
                pagination={false}
                dataSource={draft.fields}
                columns={[
                  {
                    title: "启用",
                    width: 60,
                    render: (_, f) => <Switch size="small" checked={f.enabled} onChange={(v) => updateField(f.id, { enabled: v })} />,
                  },
                  {
                    title: "字段",
                    width: 130,
                    render: (_, f) => <Input size="small" value={f.label} placeholder="如 姓名" onChange={(e) => updateField(f.id, { label: e.target.value })} />,
                  },
                  {
                    title: "类型",
                    width: 130,
                    render: (_, f) => (
                      <Select size="small" style={{ width: "100%" }} value={f.entity_type} options={ENTITY_OPTIONS} onChange={(v) => updateField(f.id, { entity_type: v })} />
                    ),
                  },
                  {
                    title: <span>真实写法 <Typography.Text type="secondary" style={{ fontSize: 11 }}>（可多个，回车分隔）</Typography.Text></span>,
                    render: (_, f) => (
                      <Select
                        size="small"
                        mode="tags"
                        style={{ width: "100%" }}
                        value={f.real_values}
                        tokenSeparators={["\n", ","]}
                        placeholder="张三 / Zhang San / zhangsan"
                        open={false}
                        onChange={(v) => updateField(f.id, { real_values: v })}
                      />
                    ),
                  },
                  {
                    title: "隐身值（模型看到的）",
                    width: 200,
                    render: (_, f) => <Input size="small" className="mono" value={f.alias} placeholder="李四" onChange={(e) => updateField(f.id, { alias: e.target.value })} />,
                  },
                  {
                    title: "忽略大小写",
                    width: 90,
                    render: (_, f) => <Switch size="small" checked={f.case_insensitive} onChange={(v) => updateField(f.id, { case_insensitive: v })} />,
                  },
                  {
                    title: "",
                    width: 40,
                    render: (_, f) => <a style={{ color: "#dc2626" }} onClick={() => update({ fields: draft.fields.filter((x) => x.id !== f.id) })}>删</a>,
                  },
                ]}
                footer={() => (
                  <Space>
                    <Button size="small" icon={<PlusOutlined />} onClick={addField}>添加字段</Button>
                    <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                      有效字段 {usable} / {draft.fields.length}（需同时填写真实写法与隐身值）
                    </Typography.Text>
                  </Space>
                )}
              />

              <Typography.Title level={5} style={{ marginTop: 16 }}>预览</Typography.Title>
              <Typography.Paragraph type="secondary" style={{ fontSize: 12 }}>
                文本中的【字段名】会自动替换为该字段的第一个真实写法。预览使用独立映射表，不影响真实会话。
              </Typography.Paragraph>
              <Input.TextArea rows={3} value={previewText} onChange={(e) => setPreviewText(e.target.value)} className="selectable" />
              <Button style={{ marginTop: 8 }} icon={<ExperimentOutlined />} loading={busy} onClick={runPreview}>用当前草稿预览</Button>
              {preview && (
                <div style={{ marginTop: 12 }}>
                  <Typography.Text type="secondary">上游将看到（命中 {preview.spans.length} 处）：</Typography.Text>
                  <pre className="pem selectable" style={{ whiteSpace: "pre-wrap" }}>{preview.redacted}</pre>
                  <Space wrap>
                    {preview.spans.map((s, i) => (
                      <Tag key={i} color={s.rule_id.startsWith("identity.") ? "purple" : "blue"}>
                        {s.rule_id.startsWith("identity.") ? "身份" : s.rule_id} → {s.entity_type}
                      </Tag>
                    ))}
                  </Space>
                </div>
              )}
            </Card>
          )}
        </Col>
      </Row>
    </div>
  );
}
