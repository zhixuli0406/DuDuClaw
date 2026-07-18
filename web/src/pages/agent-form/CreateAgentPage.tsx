import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { useAgentsStore } from '@/stores/agents-store';
import { useSystemStore } from '@/stores/system-store';
import { cn } from '@/lib/utils';
import {
  api,
  type TemplateRoster,
  type TemplateRoleDetail,
  type DepartmentInfo,
} from '@/lib/api';
import type { SelectOption } from '@/components/settings/controls';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  Badge,
  Textarea,
  BreadcrumbHeader,
  SettingsSection,
  SettingsCard,
} from '@/components/mds';
import { RowText, RowSelect, FieldBlock } from './form-rows';
import { TEMPLATE_KIND_ORDER } from './defaults';

/**
 * CreateAgentPage — standalone route (/agents/new) for hiring a new AI
 * employee. Rebuilt for WP2.3b in the Multica language: a BreadcrumbHeader over
 * a `max-w-3xl` form. The template pack picker became an mds Card grid (selected
 * card gets `ring-2 ring-brand/25`); the org placement and TOML overrides moved
 * to SettingsRow / FieldBlock. The flow (template roster → optional org wiring →
 * TOML overrides) is unchanged.
 */
export function CreateAgentPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const { agents, fetchAgents } = useAgentsStore();
  const [name, setName] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [role, setRole] = useState('specialist');
  const [trigger, setTrigger] = useState('');
  // Org placement — '' keeps the default (standalone, or the template's wiring).
  const [reportsTo, setReportsTo] = useState('');
  const [department, setDepartment] = useState('');
  const [departments, setDepartments] = useState<DepartmentInfo[]>([]);
  // Personal edition has no departments (single-owner form factor) — hide the
  // department picker there. Enterprise keeps it.
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // ── Template pack state (silent degrade when the RPC is unavailable) ──
  const [roster, setRoster] = useState<TemplateRoster | null>(null);
  const [templateRoleId, setTemplateRoleId] = useState('');
  const [roleDetail, setRoleDetail] = useState<TemplateRoleDetail | null>(null);
  const [roleLoading, setRoleLoading] = useState(false);
  const [soulMd, setSoulMd] = useState('');
  const [contractToml, setContractToml] = useState('');
  const [agentToml, setAgentToml] = useState('');
  // Only user-edited TOML is sent — untouched ⇒ backend uses template defaults.
  const [contractTouched, setContractTouched] = useState(false);
  const [agentTomlTouched, setAgentTomlTouched] = useState(false);

  useEffect(() => {
    let alive = true;
    api.templates
      .roster()
      // Shape-check the payload — a malformed/foreign response degrades to the
      // plain form instead of crashing the page render.
      .then((r) => alive && setRoster(Array.isArray(r?.roles) ? r : null))
      .catch(() => {/* no templates ⇒ plain form */});
    api.departments
      .list()
      .then((r) => alive && setDepartments(Array.isArray(r?.departments) ? r.departments : []))
      .catch(() => {/* no registry access ⇒ dropdown stays 無部門-only */});
    return () => {
      alive = false;
    };
  }, []);

  // Deep-link support: the 直屬上級 dropdown lists existing agents — make sure
  // the roster is loaded even when this page is the first one visited.
  useEffect(() => {
    if (agents.length === 0) void fetchAgents();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const sortedRoles = roster
    ? [...roster.roles].sort((a, b) => TEMPLATE_KIND_ORDER[a.kind] - TEMPLATE_KIND_ORDER[b.kind])
    : [];

  const selectTemplate = async (roleId: string) => {
    setTemplateRoleId(roleId);
    setRoleDetail(null);
    setContractTouched(false);
    setAgentTomlTouched(false);
    setReportsTo(''); // back to the template's own wiring
    setDepartment('');
    if (roleId === '') return;
    setRoleLoading(true);
    setError(null);
    try {
      const d = await api.templates.role(roleId, roster?.industry ?? undefined);
      setRoleDetail(d);
      setName(d.name);
      setDisplayName(d.display_name);
      setTrigger(d.trigger);
      setSoulMd(d.soul_md);
      setContractToml(d.contract_toml);
      setAgentToml(d.agent_toml);
    } catch (e) {
      setTemplateRoleId('');
      setError(formatError(e));
    } finally {
      setRoleLoading(false);
    }
  };

  const usingTemplate = templateRoleId !== '' && roleDetail !== null;

  const handleSubmit = async () => {
    if (!name.trim() || !displayName.trim()) return;
    setError(null);
    setSubmitting(true);
    try {
      if (usingTemplate) {
        const res = await api.templates.createAgent({
          role_id: templateRoleId,
          ...(roster?.industry ? { industry: roster.industry } : {}),
          name: name.trim(),
          display_name: displayName.trim(),
          trigger: trigger || `@${displayName.trim()}`,
          ...(reportsTo ? { reports_to: reportsTo } : {}),
          ...(department ? { department } : {}),
          soul_md: soulMd,
          ...(contractTouched ? { contract_toml: contractToml } : {}),
          ...(agentTomlTouched ? { agent_toml: agentToml } : {}),
        });
        if (res.warning) toast.info(res.warning);
      } else {
        await api.agents.create({
          name: name.trim(),
          display_name: displayName.trim(),
          role,
          trigger: trigger || `@${displayName.trim()}`,
          ...(reportsTo ? { reports_to: reportsTo } : {}),
          ...(department ? { department } : {}),
        });
      }
      // Same post-create behavior as the former dialog's onCreated: refresh
      // the roster, then return to the list.
      await fetchAgents();
      navigate('/agents');
    } catch (e) {
      // Template errors carry an actionable zh-TW message (e.g. bad TOML).
      setError(usingTemplate ? formatError(e) : intl.formatMessage({ id: 'agents.create.error' }));
    } finally {
      setSubmitting(false);
    }
  };

  // Human-kept posts / deliberately-not-deployed kits — only meaningful when
  // an industry-pack role is selected (the generic CEO roster has neither).
  const showRosterNotes = usingTemplate && roster?.industry != null;

  const t = (key: string) => intl.formatMessage({ id: key });

  const roleOptions: SelectOption[] = ['main', 'specialist', 'worker', 'developer', 'qa', 'planner'].map(
    (r) => ({ value: r, label: t(`agents.role.${r}`), raw: r }),
  );
  const reportsToOptions: SelectOption[] = [
    {
      value: '',
      label:
        usingTemplate && roleDetail?.reports_to
          ? intl.formatMessage({ id: 'agents.create.reportsTo.templateDefault' }, { name: roleDetail.reports_to })
          : t('agents.create.reportsTo.none'),
    },
    ...agents
      .filter((a) => a.name !== name.trim())
      .map((a) => ({
        value: a.name,
        label: a.display_name && a.display_name !== a.name ? `${a.display_name} · ${a.name}` : a.name,
        raw: a.name,
      })),
  ];
  const departmentOptions: SelectOption[] = [
    { value: '', label: t('agents.create.department.none') },
    ...departments.map((d) => ({ value: d.name, label: d.name, raw: d.name })),
  ];

  const canSubmit = !submitting && !roleLoading && !!name.trim() && !!displayName.trim();

  return (
    <div className="-mx-4 -mt-4 -mb-20 flex min-h-0 flex-1 flex-col md:-mx-6 md:-mt-6 md:-mb-6">
      <BreadcrumbHeader
        segments={[
          { label: t('nav.agents'), onClick: () => navigate('/agents') },
          { label: t('agents.create') },
        ]}
        actions={
          <>
            <Button variant="ghost" size="sm" onClick={() => navigate('/agents')}>
              {t('common.cancel')}
            </Button>
            <Button variant="brand" size="sm" onClick={handleSubmit} disabled={!canSubmit}>
              {submitting ? t('common.loading') : t('agents.create')}
            </Button>
          </>
        }
      />

      <div className="mx-auto w-full max-w-3xl flex-1 space-y-8 overflow-y-auto p-4 md:p-8">
        {/* Template pack picker — mds Card grid. */}
        {roster && sortedRoles.length > 0 && (
          <SettingsSection title={t('agents.create.section.template')}>
            <div className="grid gap-3 sm:grid-cols-2">
              <TemplateCard
                title={t('agents.create.template.blank')}
                description={t('agents.create.template.blankDesc')}
                selected={templateRoleId === ''}
                onSelect={() => void selectTemplate('')}
              />
              {sortedRoles.map((r) => (
                <TemplateCard
                  key={r.role_id}
                  title={r.display_name}
                  description={r.summary || undefined}
                  selected={templateRoleId === r.role_id}
                  disabled={r.created}
                  badge={r.created ? t('agents.create.template.created') : undefined}
                  onSelect={() => void selectTemplate(r.role_id)}
                />
              ))}
            </div>

            {showRosterNotes && roster && (roster.humans.length > 0 || roster.excluded.length > 0) && (
              <div className="space-y-1 rounded-lg bg-muted/50 p-3 text-xs text-muted-foreground">
                {roster.humans.length > 0 && (
                  <p>
                    {intl.formatMessage(
                      { id: 'agents.create.template.humans' },
                      { titles: intl.formatList(roster.humans.map((h) => h.title), { type: 'unit' }) },
                    )}
                  </p>
                )}
                {roster.excluded.length > 0 && (
                  <p>
                    {intl.formatMessage(
                      { id: 'agents.create.template.excluded' },
                      {
                        items: intl.formatList(
                          roster.excluded.map((x) =>
                            intl.formatMessage({ id: 'agents.create.template.excludedItem' }, { kit: x.kit, reason: x.reason }),
                          ),
                          { type: 'unit' },
                        ),
                      },
                    )}
                  </p>
                )}
              </div>
            )}

            {roleLoading && (
              <p className="text-sm text-muted-foreground">{t('agents.create.template.loading')}</p>
            )}
          </SettingsSection>
        )}

        {/* Basics. */}
        <SettingsSection title={t('agents.create.section.basics')}>
          <SettingsCard>
            <RowText label={t('agents.create.idLabel')} description={t('agents.create.idHint')} value={name} placeholder="coder" onChange={setName} />
            <RowText label={t('agents.create.displayName')} value={displayName} placeholder="Coder" onChange={setDisplayName} />
            {!usingTemplate && (
              <RowSelect label={t('orgchart.detail.role')} value={role} onChange={setRole} options={roleOptions} />
            )}
            <RowText label={t('orgchart.detail.trigger')} description={t('agents.create.triggerHint')} value={trigger} placeholder="@Coder" onChange={setTrigger} />
          </SettingsCard>
        </SettingsSection>

        {/* Organization. */}
        <SettingsSection title={t('agents.create.section.org')}>
          <SettingsCard>
            <RowSelect label={t('agents.create.reportsTo')} description={t('agents.create.reportsToHint')} value={reportsTo} onChange={setReportsTo} options={reportsToOptions} />
            {!isPersonal && (
              <RowSelect label={t('agents.department.label')} description={t('agents.create.departmentHint')} value={department} onChange={setDepartment} options={departmentOptions} />
            )}
          </SettingsCard>
        </SettingsSection>

        {/* Template overrides. */}
        {usingTemplate && (
          <SettingsSection title={t('agents.create.template.soul')}>
            <FieldBlock>
              <Textarea
                value={soulMd}
                onChange={(e) => setSoulMd(e.target.value)}
                spellCheck={false}
                className="min-h-56 resize-y font-mono leading-relaxed"
              />
            </FieldBlock>
            <details className="rounded-lg border border-surface-border p-3">
              <summary className="cursor-pointer text-sm font-medium text-foreground">
                {t('agents.create.template.advanced')}
              </summary>
              <div className="mt-3 space-y-4">
                <FieldBlock label={t('agents.create.template.contract')}>
                  <Textarea
                    value={contractToml}
                    onChange={(e) => { setContractToml(e.target.value); setContractTouched(true); }}
                    spellCheck={false}
                    className="min-h-32 resize-y font-mono leading-relaxed"
                  />
                </FieldBlock>
                <FieldBlock label={t('agents.create.template.agentToml')}>
                  <Textarea
                    value={agentToml}
                    onChange={(e) => { setAgentToml(e.target.value); setAgentTomlTouched(true); }}
                    spellCheck={false}
                    className="min-h-32 resize-y font-mono leading-relaxed"
                  />
                </FieldBlock>
              </div>
            </details>
          </SettingsSection>
        )}

        {error && <p className="text-sm text-destructive">{error}</p>}
      </div>
    </div>
  );
}

/** Selectable template card (spec §5.3b — selected gets `ring-2 ring-brand/25`). */
function TemplateCard({
  title,
  description,
  selected,
  disabled,
  badge,
  onSelect,
}: {
  title: string;
  description?: string;
  selected: boolean;
  disabled?: boolean;
  badge?: string;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      disabled={disabled}
      aria-pressed={selected}
      className={cn(
        'flex flex-col gap-1 rounded-xl border border-surface-border bg-surface p-3 text-left shadow-[var(--surface-shadow)] outline-none transition-colors',
        'hover:bg-surface-hover focus-visible:ring-3 focus-visible:ring-ring/50',
        'disabled:cursor-not-allowed disabled:opacity-50',
        selected && 'border-brand/40 ring-2 ring-brand/25',
      )}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="truncate text-sm font-medium text-foreground">{title}</span>
        {badge && <Badge variant="secondary">{badge}</Badge>}
      </div>
      {description && (
        <span className="line-clamp-2 text-xs text-muted-foreground">{description}</span>
      )}
    </button>
  );
}
