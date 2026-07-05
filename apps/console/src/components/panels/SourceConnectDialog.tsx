import { useState } from "react";
import { ArrowLeft, ExternalLink, Loader2 } from "lucide-react";

import { useClients } from "@/lib/connection";
import { errMsg, tableNameString } from "@/lib/format";
import { streamQuery } from "@/lib/query";
import type { AuthMode, Connector } from "@/catalog";
import { ConnectorIcon } from "@/components/ConnectorIcon";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Field } from "@/components/ui/field";
import { Collapsible } from "@/components/ui/collapsible";
import { Segmented, type SegmentedOption } from "@/components/ui/segmented";
import {
  Dialog,
  DialogContent,
  DialogTitle,
} from "@/components/ui/dialog";

const AUTH_LABELS: Record<AuthMode, string> = {
  api_token: "API Token",
  oauth_org: "OAuth (Org-level)",
  oauth_member: "OAuth (Per-member)",
  none: "None",
};

const IDENT_RE = /^[a-z_][a-z0-9_]*$/i;

type TestState =
  | { kind: "idle" }
  | { kind: "running" }
  | { kind: "ok"; detail: string }
  | { kind: "err"; detail: string };

export function SourceConnectDialog({
  connector,
  onBack,
  onClose,
  onSaved,
}: {
  connector: Connector | null;
  onBack: () => void;
  onClose: () => void;
  onSaved: (name: string) => void;
}) {
  return (
    <Dialog
      open={!!connector}
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogContent className="max-w-xl">
        {connector ? (
          <ConnectForm
            connector={connector}
            onBack={onBack}
            onClose={onClose}
            onSaved={onSaved}
          />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

function ConnectForm({
  connector,
  onBack,
  onClose,
  onSaved,
}: {
  connector: Connector;
  onBack: () => void;
  onClose: () => void;
  onSaved: (name: string) => void;
}) {
  const { sources, catalog, query } = useClients();
  const [name, setName] = useState(connector.id);
  const [description, setDescription] = useState("");
  const [authMode, setAuthMode] = useState<AuthMode>(connector.authModes[0]);
  const [values, setValues] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [test, setTest] = useState<TestState>({ kind: "idle" });

  const showAuth = connector.authModes.length > 1;
  const isOauth = authMode === "oauth_org" || authMode === "oauth_member";
  const authOptions: SegmentedOption<AuthMode>[] = connector.authModes.map(
    (m) => ({ value: m, label: AUTH_LABELS[m] }),
  );

  const main = connector.fields.filter((f) => !f.advanced);
  const advanced = connector.fields.filter((f) => f.advanced);

  const nameValid = IDENT_RE.test(name);
  const missingRequired = !isOauth
    ? connector.fields.some((f) => f.required && !values[f.name]?.trim())
    : false;
  const canSubmit = nameValid && !isOauth && !missingRequired;

  function set(field: string, v: string) {
    setValues((prev) => ({ ...prev, [field]: v }));
  }

  /** Add under a throwaway name, probe a filter-free table, then remove. */
  async function handleTest() {
    setTest({ kind: "running" });
    setError(null);
    const probe = `probe_${name}`.slice(0, 60);
    let added = false;
    try {
      const yaml = connector.buildYaml({ name: probe, description, values });
      await sources.addSource({ yaml });
      added = true;
      const tables = (await catalog.listTables({ source: probe })).tables;
      const probeable = tables.find(
        (t) => t.name && t.requiredFilters.length === 0,
      );
      if (!probeable?.name) {
        setTest({
          kind: "ok",
          detail: tables.length
            ? `Registered — ${tables.length} tables (none probe-able without filters).`
            : "Registered.",
        });
        return;
      }
      const ref = tableNameString(probeable.name);
      const res = await streamQuery(query, `SELECT * FROM ${ref} LIMIT 1`, 1, {});
      setTest({
        kind: "ok",
        detail: `Connected — ${ref} returned ${res.rows.length} row${res.rows.length === 1 ? "" : "s"}.`,
      });
    } catch (e) {
      setTest({ kind: "err", detail: errMsg(e) });
    } finally {
      if (added) await sources.removeSource({ name: probe }).catch(() => {});
    }
  }

  async function handleSave() {
    if (!canSubmit) return;
    setSaving(true);
    setError(null);
    try {
      const yaml = connector.buildYaml({ name, description, values });
      await sources.addSource({ yaml });
      onSaved(name);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-3">
        <Button
          variant="outline"
          size="icon"
          className="size-8 rounded-full"
          onClick={onBack}
          aria-label="Back to gallery"
        >
          <ArrowLeft className="size-4" />
        </Button>
        <ConnectorIcon seed={connector.id} label={connector.label} className="size-8" />
        <DialogTitle>Connect to {connector.label}</DialogTitle>
      </div>

      <div className="bg-muted/40 flex items-center justify-between gap-3 rounded-lg px-3 py-2.5">
        <div className="flex items-center gap-3">
          <ConnectorIcon seed={connector.id} label={connector.label} />
          <div className="leading-tight">
            <div className="text-sm font-semibold">{connector.label}</div>
            <div className="text-muted-foreground text-xs">{connector.subtitle}</div>
          </div>
        </div>
        {connector.docsUrl ? (
          <a
            href={connector.docsUrl}
            target="_blank"
            rel="noreferrer"
            className="text-primary inline-flex items-center gap-1 text-xs hover:underline"
          >
            Docs <ExternalLink className="size-3" />
          </a>
        ) : null}
      </div>

      <div className="max-h-[55vh] space-y-4 overflow-y-auto pr-1">
        <Field
          label="Name"
          required
          htmlFor="connector-name"
          error={
            name && !nameValid
              ? "Use a SQL identifier: letters, digits, underscore."
              : undefined
          }
          help="The SQL schema prefix for this source's tables."
        >
          <Input
            id="connector-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            spellCheck={false}
            autoComplete="off"
          />
        </Field>

        <Field label="Description" htmlFor="connector-desc">
          <Input
            id="connector-desc"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder="Optional description"
          />
        </Field>

        {showAuth ? (
          <div className="space-y-1.5">
            <span className="block text-sm font-medium">Authentication Type</span>
            <Segmented options={authOptions} value={authMode} onChange={setAuthMode} />
          </div>
        ) : null}

        {isOauth ? (
          <div className="border-warning/30 bg-warning/5 text-foreground rounded-lg border p-3 text-sm">
            Browser-driven OAuth isn't available yet. Save with API Token, or after
            saving connect from a terminal:
            <pre className="bg-terminal text-terminal-foreground mt-2 overflow-auto rounded-md p-2 font-mono text-xs">
              {`pawrly source connect ${name}`}
            </pre>
          </div>
        ) : (
          <>
            {main.map((f) => (
              <Field
                key={f.name}
                label={f.label}
                required={f.required}
                htmlFor={`f-${f.name}`}
                help={f.help}
              >
                <Input
                  id={`f-${f.name}`}
                  type={f.type === "password" ? "password" : f.type === "number" ? "number" : "text"}
                  value={values[f.name] ?? ""}
                  onChange={(e) => set(f.name, e.target.value)}
                  placeholder={f.placeholder}
                  spellCheck={false}
                  autoComplete="off"
                />
              </Field>
            ))}
            {advanced.length > 0 ? (
              <Collapsible label="Advanced">
                {advanced.map((f) => (
                  <Field
                    key={f.name}
                    label={f.label}
                    required={f.required}
                    htmlFor={`f-${f.name}`}
                    help={f.help}
                  >
                    <Input
                      id={`f-${f.name}`}
                      type={f.type === "password" ? "password" : f.type === "number" ? "number" : "text"}
                      value={values[f.name] ?? ""}
                      onChange={(e) => set(f.name, e.target.value)}
                      placeholder={f.placeholder}
                      spellCheck={false}
                      autoComplete="off"
                    />
                  </Field>
                ))}
              </Collapsible>
            ) : null}
          </>
        )}

        {test.kind === "ok" ? (
          <p className="text-success text-sm">{test.detail}</p>
        ) : test.kind === "err" ? (
          <p className="text-destructive font-mono text-xs break-all">{test.detail}</p>
        ) : null}
        {error ? (
          <p className="text-destructive font-mono text-xs break-all">{error}</p>
        ) : null}
      </div>

      <div className="flex items-center justify-between gap-2 border-t pt-4">
        <Button
          variant="outline"
          onClick={handleTest}
          disabled={!canSubmit || test.kind === "running" || saving}
        >
          {test.kind === "running" ? (
            <Loader2 className="size-4 animate-spin" />
          ) : null}
          Test Connection
        </Button>
        <div className="flex items-center gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button onClick={handleSave} disabled={!canSubmit || saving}>
            {saving ? <Loader2 className="size-4 animate-spin" /> : null}
            Save
          </Button>
        </div>
      </div>
    </div>
  );
}
