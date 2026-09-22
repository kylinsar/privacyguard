import React, { useEffect, useMemo, useState } from "react";
import {
  Button, Card, Col, Drawer, Form, Input, InputNumber, Popconfirm, Row, Select, Space, Switch, Table, Tag, Typography,
  App as AntApp,
} from "antd";
import { PlusOutlined, ExperimentOutlined, ImportOutlined, ExportOutlined } from "@ant-design/icons";
import { save, open } from "@tauri-apps/plugin-dialog";
import { api, type Rule, type RuleTestResult } from "../api";

const SAMPLE = `联系人：张三 <zhangsan@example.com>，手机 13812345678
身份证 110101199003074512，卡号 4111 1111 1111 1111
API_KEY=sk-proj-abcdefghijklmnopqrstuvwxyz123456
Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U`;

export default function Rules() {
  const [rules, setRules] = useState<Rule[]>([]);
  const [editing, setEditing] = useState<Rule | null>(null);
  const [testText, setTestText] = useState(SAMPLE);
  const [testResult, setTestResult] = useState<RuleTestResult | null>(null);
  const [testing, setTesting] = useState(false);
  const { message } = AntApp.useApp();
  const [form] = Form.useForm<Rule>();

  const load = () => api.listRules().then(setRules).catch((e) => message.error(String(e)));
  useEffect(() => {
    load();
  }, []);

  const runTest = async (draft?: Rule) => {
    setTesting(true);
    try {
      setTestResult(await api.testRules(testText, draft));
    } catch (e) {
      message.error(String(e), 6);
    } finally {
      setTesting(false);
    }
  };

  const openEditor = (r?: Rule) => {
    const base: Rule = r ?? {
      id: "custom." + Math.random().toString(36).slice(2, 8),
      name: "",
      entity_type: "CUSTOM",
      kind: "regex",
      pattern: "",
      group: null,
      enabled: true,
      builtin: false,
      priority: 10,
      validator: null,
      description: "",
    };
    setEditing(base);
    form.setFieldsValue(base);
  };

  const saveRule = async () => {
    const values = await form.validateFields();
    const rule: Rule = { ...editing!, ...values, builtin: false, kind: "regex" };
    try {
      setRules(await api.upsertRule(rule));
      message.success("已保存");
      setEditing(null);
    } catch (e) {
      message.error(String(e), 6);
    }
  };

  const exportRules = async () => {
    const dest = await save({ defaultPath: "privacyguard-rules.json", filters: [{ name: "JSON", extensions: ["json"] }] });
    if (!dest) return;
    try {
      const n = await api.exportRules(dest);
      message.success(`已导出 ${n} 条自定义规则`);
    } catch (e) {
      message.error(String(e), 6);
    }
  };

  const importRules = async () => {
    const path = await open({ multiple: false, filters: [{ name: "JSON", extensions: ["json"] }] });
    if (!path) return;
    try {
      setRules(await api.importRules(path as string));
      message.success("导入完成（内置规则条目会被跳过）");
    } catch (e) {
      message.error(String(e), 6);
    }
  };

  const highlighted = useMemo(() => {
    if (!testResult) return null;
    const parts: React.ReactNode[] = [];
    let last = 0;
    const bytes = new TextEncoder().encode(testText);
    const dec = new TextDecoder();
    // span 偏移是 UTF-8 字节偏移，需要按字节切分
    testResult.spans.forEach((s, i) => {
      parts.push(<span key={"t" + i}>{dec.decode(bytes.slice(last, s.start))}</span>);
      parts.push(
        <span key={"h" + i} className="hl" title={`${s.rule_id} → ${s.entity_type}`}>
          {dec.decode(bytes.slice(s.start, s.end))}
        </span>,
      );
      last = s.end;
    });
    parts.push(<span key="end">{dec.decode(bytes.slice(last))}</span>);
    return parts;
  }, [testResult, testText]);

  return (
    <div>
      <h1 className="pg-page-title">脱敏规则</h1>
      <Row gutter={16}>
        <Col span={14}>
          <Card
            title={`规则列表（${rules.filter((r) => r.enabled).length}/${rules.length} 启用）`}
            extra={
              <Space>
                <Button icon={<ImportOutlined />} onClick={importRules}>导入</Button>
                <Button icon={<ExportOutlined />} onClick={exportRules}>导出自定义</Button>
                <Button type="primary" icon={<PlusOutlined />} onClick={() => openEditor()}>新建规则</Button>
              </Space>
            }
          >
            <Table
              size="small"
              rowKey="id"
              pagination={false}
              dataSource={rules}
              columns={[
                {
                  title: "启用",
                  width: 70,
                  render: (_, r) => (
                    <Switch
                      size="small"
                      checked={r.enabled}
                      onChange={async (v) => {
                        try {
                          setRules(await api.setRuleEnabled(r.id, v));
                        } catch (e) {
                          message.error(String(e));
                        }
                      }}
                    />
                  ),
                },
                {
                  title: "名称",
                  render: (_, r) => (
                    <span>
                      {r.name} {r.builtin ? <Tag>内置</Tag> : <Tag color="purple">自定义</Tag>}
                      {r.description && <div style={{ color: "#94a3b8", fontSize: 11 }}>{r.description}</div>}
                    </span>
                  ),
                },
                { title: "实体", dataIndex: "entity_type", width: 110, render: (t) => <Tag color="blue">{t}</Tag> },
                { title: "优先级", dataIndex: "priority", width: 70 },
                {
                  title: "模式",
                  dataIndex: "pattern",
                  ellipsis: true,
                  render: (p) => <span className="mono" style={{ fontSize: 11 }}>{p}</span>,
                },
                {
                  title: "",
                  width: 110,
                  render: (_, r) =>
                    r.builtin ? null : (
                      <Space size={4}>
                        <a onClick={() => openEditor(r)}>编辑</a>
                        <Popconfirm title="删除该规则？" onConfirm={async () => setRules(await api.deleteRule(r.id))}>
                          <a style={{ color: "#dc2626" }}>删除</a>
                        </Popconfirm>
                      </Space>
                    ),
                },
              ]}
            />
          </Card>
        </Col>
        <Col span={10}>
          <Card
            title="测试面板"
            extra={
              <Button icon={<ExperimentOutlined />} type="primary" loading={testing} onClick={() => runTest()}>
                运行
              </Button>
            }
          >
            <Input.TextArea rows={7} value={testText} onChange={(e) => setTestText(e.target.value)} className="mono selectable" style={{ fontSize: 12 }} />
            {testResult && (
              <div style={{ marginTop: 12 }}>
                <Typography.Text type="secondary">命中 {testResult.spans.length} 处</Typography.Text>
                <div className="selectable" style={{ whiteSpace: "pre-wrap", fontSize: 12, marginTop: 6, lineHeight: 1.7 }}>{highlighted}</div>
                <Typography.Text type="secondary" style={{ display: "block", marginTop: 12 }}>上游将看到：</Typography.Text>
                <pre className="pem selectable" style={{ whiteSpace: "pre-wrap" }}>{testResult.redacted}</pre>
                <Space wrap>
                  {testResult.spans.map((s, i) => (
                    <Tag key={i}>{s.rule_id}</Tag>
                  ))}
                </Space>
              </div>
            )}
          </Card>
        </Col>
      </Row>

      <Drawer
        title={editing?.builtin === false && rules.some((r) => r.id === editing.id) ? "编辑规则" : "新建规则"}
        open={!!editing}
        onClose={() => setEditing(null)}
        width={520}
        extra={
          <Space>
            <Button onClick={() => form.validateFields().then((v) => runTest({ ...editing!, ...v }))}>用当前草稿测试</Button>
            <Button type="primary" onClick={saveRule}>保存</Button>
          </Space>
        }
      >
        <Form form={form} layout="vertical">
          <Form.Item name="id" label="ID" rules={[{ required: true }, { pattern: /^(?!builtin\.)[\w.-]+$/, message: "不能以 builtin. 开头" }]}>
            <Input className="mono" />
          </Form.Item>
          <Form.Item name="name" label="名称" rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item name="entity_type" label="实体类型（出现在占位符中，如 PG_EMP_ID_xxxx）" rules={[{ required: true }, { pattern: /^[A-Za-z0-9_]+$/, message: "仅字母数字下划线" }]}>
            <Input className="mono" />
          </Form.Item>
          <Form.Item name="pattern" label="正则（Rust regex 语法，不支持环视）" rules={[{ required: true }]}>
            <Input.TextArea rows={3} className="mono" />
          </Form.Item>
          <Row gutter={12}>
            <Col span={8}>
              <Form.Item name="group" label="只脱敏捕获组">
                <InputNumber min={1} style={{ width: "100%" }} placeholder="整段" />
              </Form.Item>
            </Col>
            <Col span={8}>
              <Form.Item name="priority" label="优先级">
                <InputNumber style={{ width: "100%" }} />
              </Form.Item>
            </Col>
            <Col span={8}>
              <Form.Item name="validator" label="校验">
                <Select allowClear options={[{ value: "luhn", label: "Luhn（银行卡）" }]} />
              </Form.Item>
            </Col>
          </Row>
          <Form.Item name="description" label="说明">
            <Input />
          </Form.Item>
          <Form.Item name="enabled" label="启用" valuePropName="checked">
            <Switch />
          </Form.Item>
        </Form>
      </Drawer>
    </div>
  );
}
