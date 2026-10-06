import { describe, it, expect, vi, afterEach } from 'vitest';
import { screen, fireEvent, render } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Route, Routes, useLocation } from 'react-router';
import en from '@/i18n/en.json';
import { useAuthStore } from '@/stores/auth-store';

// The panel imports decideApproval → api-custom-skills → ws-client. Stub the
// socket so the module graph loads without a live connection.
vi.mock('@/lib/ws-client', () => ({
  client: {
    call: vi.fn().mockResolvedValue({}),
    // agents-store (pulled in via CharacterAvatar → wardrobe outfit lookup)
    // subscribes at module init; a no-op keeps the graph loadable offline.
    subscribe: vi.fn(),
  },
}));

import { ApprovalDetailPanel } from './ApprovalDetailPanel';
import type { ApprovalItem } from '@/lib/api';

const SKILL_MD = 'name: my-skill\ndescription: does a thing\n\n# Body\nStep one.';

function skillCreateApproval(payloadOverride?: Record<string, unknown>): ApprovalItem {
  return {
    id: 'apr-1',
    agent_id: 'agent-a',
    kind: 'skill_create',
    summary: '自建技能送審：客服待辦整理',
    created_at: '2026-07-10T00:00:00Z',
    ttl_seconds: 604800,
    payload: {
      custom_skill_id: 'cs-1',
      slug: 'my-skill',
      display_name: '客服待辦整理',
      description_human: '把對話整理成待辦',
      time_saved_value: 30,
      time_saved_unit: 'minutes_per_use',
      tags: 'ops',
      created_by_user: 'user-1',
      built_by_agent: 'agent-a',
      skill_md: SKILL_MD,
      safety_report: {
        passed: true,
        risk_level: 'Low',
        findings: [{ category: 'style', severity: 'low', description: 'prefer explicit tools list', line_number: 3 }],
        sandbox_trial: { ran: false, skip_reason: 'post-install mechanism' },
      },
      ...payloadOverride,
    },
  };
}

describe('<ApprovalDetailPanel> skill_create', () => {
  it('renders the SKILL.md artifact, safety findings, and human fields', () => {
    renderWithProviders(
      <ApprovalDetailPanel approval={skillCreateApproval()} onApprove={vi.fn()} onReject={vi.fn()} />,
    );
    // The artifact that installs on approve is shown verbatim.
    expect(screen.getByText(/name: my-skill/)).toBeInTheDocument();
    // Safety finding surfaces.
    expect(screen.getByText(/prefer explicit tools list/)).toBeInTheDocument();
    // Risk level badge.
    expect(screen.getByText('Low')).toBeInTheDocument();
    // Human display name.
    expect(screen.getByText('客服待辦整理')).toBeInTheDocument();
  });

  it('falls back to the generic view when the SKILL.md is absent', () => {
    const approval = skillCreateApproval({ skill_md: '' });
    renderWithProviders(<ApprovalDetailPanel approval={approval} onApprove={vi.fn()} onReject={vi.fn()} />);
    // The summary is shown by the generic branch; no SKILL.md pre block.
    expect(screen.getByText(approval.summary)).toBeInTheDocument();
    expect(screen.queryByText(/name: my-skill/)).not.toBeInTheDocument();
  });
});

function genericApproval(over?: Partial<ApprovalItem>): ApprovalItem {
  return {
    id: 'apr-2',
    agent_id: 'agent-b',
    kind: 'tool_call',
    summary: 'Run a shell command to tidy the logs',
    created_at: '2026-07-11T00:00:00Z',
    ttl_seconds: 3600,
    payload: { tool: 'Bash', command: 'rm /tmp/old.log', scope: 'workspace-write' },
    ...over,
  };
}

describe('<ApprovalDetailPanel> generic (U2 redesign)', () => {
  it('leads with the plan summary + a whole-action risk badge', () => {
    renderWithProviders(<ApprovalDetailPanel approval={genericApproval()} onApprove={vi.fn()} onReject={vi.fn()} />);
    // Plan-first: "What this AI employee plans to do" heading + plain-language kind.
    expect(screen.getByText('What this AI employee plans to do')).toBeInTheDocument();
    expect(screen.getByText('Call a tool to carry out an action')).toBeInTheDocument();
    // Whole-action risk badge (tool_call → medium).
    expect(screen.getByText('Medium risk')).toBeInTheDocument();
    // Scope-of-impact facts surface as verification aids.
    expect(screen.getByText('Bash')).toBeInTheDocument();
    expect(screen.getByText('workspace-write')).toBeInTheDocument();
  });

  it('keeps the raw payload behind an opt-in spot-check', () => {
    renderWithProviders(<ApprovalDetailPanel approval={genericApproval()} onApprove={vi.fn()} onReject={vi.fn()} />);
    // The full JSON payload is not force-read.
    expect(screen.queryByText(/"command": "rm \/tmp\/old.log"/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByText('Spot-check full details'));
    expect(screen.getByText(/"command": "rm \/tmp\/old.log"/)).toBeInTheDocument();
  });

  it('approves medium-risk actions directly', () => {
    const onApprove = vi.fn();
    renderWithProviders(<ApprovalDetailPanel approval={genericApproval()} onApprove={onApprove} onReject={vi.fn()} />);
    fireEvent.click(screen.getByText('Approve'));
    expect(onApprove).toHaveBeenCalledTimes(1);
  });

  it('gates high-risk approval behind a second confirmation', () => {
    const onApprove = vi.fn();
    const approval = genericApproval({ kind: 'agent_hire', summary: 'Hire a data-entry AI employee' });
    renderWithProviders(<ApprovalDetailPanel approval={approval} onApprove={onApprove} onReject={vi.fn()} />);
    expect(screen.getByText('High risk')).toBeInTheDocument();
    // First click opens the confirm dialog — it does NOT approve yet.
    fireEvent.click(screen.getByText('Approve'));
    expect(onApprove).not.toHaveBeenCalled();
    // Confirming in the dialog runs the approval exactly once.
    fireEvent.click(screen.getByText('Confirm approve'));
    expect(onApprove).toHaveBeenCalledTimes(1);
  });
});

function ReviewLocation() {
  const location = useLocation();
  return <span data-testid="review-location">{location.pathname + location.search}</span>;
}

describe('<ApprovalDetailPanel> synthetic run inspection', () => {
  const exactPayload = {
    tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'synthetic-support-47-35',
    scenario_id: 'dashboard-synthetic-baseline-two-agents-47-35', replay_hash: 'a'.repeat(64),
  };
  const approvalId = '123e4567-e89b-42d3-a456-426614174000';

  // The Decision Lab deep link this describe block exercises is admin-only
  // (`RoleGuard minRole="admin"` on `/app/system/decision-lab`), so these
  // tests must set an explicit role each time rather than rely on the auth
  // store's default `user: null` — restored afterwards so it doesn't leak
  // into unrelated describe blocks in this file.
  afterEach(() => {
    useAuthStore.setState({ user: null });
  });

  function setReviewerRole(role: 'admin' | 'manager' | 'employee') {
    useAuthStore.setState({
      user: { id: 'reviewer-1', email: 'reviewer@example.com', display_name: 'Reviewer', role, status: 'active' },
    });
  }

  it('offers a same-origin exact-run link without deciding the approval (admin reviewer)', () => {
    setReviewerRole('admin');
    const onApprove = vi.fn();
    render(<IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={['/inbox']}><Routes>
        <Route path="/inbox" element={<ApprovalDetailPanel approval={genericApproval({
          id: approvalId, kind: 'support_pilot_review', payload: exactPayload,
        })} onApprove={onApprove} onReject={vi.fn()} />} />
        <Route path="/app/system/decision-lab" element={<ReviewLocation />} />
      </Routes></MemoryRouter>
    </IntlProvider>);
    fireEvent.click(screen.getByRole('button', { name: 'Inspect exact run in Decision Lab' }));
    expect(screen.getByTestId('review-location')).toHaveTextContent(/^\/app\/system\/decision-lab\?/);
    expect(screen.getByTestId('review-location')).toHaveTextContent('expected_hash=' + 'a'.repeat(64));
    expect(onApprove).not.toHaveBeenCalled();
  });

  // Regression (review_misc.md task 2 / W3-5): the deep link used to render
  // unconditionally for every role even though `/app/system/decision-lab`
  // itself is `RoleGuard minRole="admin"` — a non-admin reviewer clicking it
  // was silently bounced back to `/` with no explanation. A non-admin now
  // sees a plain-language note instead of a dead-end button.
  it('shows an admin-only note instead of a dead-end link for a non-admin reviewer', () => {
    setReviewerRole('employee');
    renderWithProviders(<ApprovalDetailPanel approval={genericApproval({
      id: approvalId, kind: 'support_pilot_review', payload: exactPayload,
    })} onApprove={vi.fn()} onReject={vi.fn()} />);
    expect(screen.queryByRole('button', { name: 'Inspect exact run in Decision Lab' })).not.toBeInTheDocument();
    expect(screen.getByText('Requires an administrator to review in Decision Lab.')).toBeInTheDocument();
  });

  it('shows no deep link for a malformed broker payload', () => {
    setReviewerRole('admin');
    renderWithProviders(<ApprovalDetailPanel approval={genericApproval({
      id: approvalId, kind: 'support_pilot_review', payload: { ...exactPayload, replay_hash: 'bad' },
    })} onApprove={vi.fn()} onReject={vi.fn()} />);
    expect(screen.queryByRole('button', { name: 'Inspect exact run in Decision Lab' })).not.toBeInTheDocument();
  });
});

describe('<ApprovalDetailPanel> D1/D2 simulation trajectory', () => {
  it('renders nothing when simulation is absent (backward compat)', () => {
    renderWithProviders(<ApprovalDetailPanel approval={genericApproval()} onApprove={vi.fn()} onReject={vi.fn()} />);
    expect(screen.queryByText('If approved, what happens next')).not.toBeInTheDocument();
  });

  it('renders nothing when simulation fields are both blank/empty', () => {
    const approval = genericApproval({ simulation: { world_state_change: '   ', risk_points: [] } });
    renderWithProviders(<ApprovalDetailPanel approval={approval} onApprove={vi.fn()} onReject={vi.fn()} />);
    expect(screen.queryByText('If approved, what happens next')).not.toBeInTheDocument();
  });

  it('renders the narrative and risk points when simulation is present', () => {
    const approval = genericApproval({
      simulation: {
        world_state_change: 'A refund email will be sent to the customer.',
        risk_points: ['Amount miscalculation is hard to reverse'],
      },
    });
    renderWithProviders(<ApprovalDetailPanel approval={approval} onApprove={vi.fn()} onReject={vi.fn()} />);
    expect(screen.getByText('If approved, what happens next')).toBeInTheDocument();
    expect(screen.getByText('A refund email will be sent to the customer.')).toBeInTheDocument();
    expect(screen.getByText('Risks to watch for')).toBeInTheDocument();
    expect(screen.getByText('Amount miscalculation is hard to reverse')).toBeInTheDocument();
  });
});

describe('<ApprovalDetailPanel> TTL countdown', () => {
  it('does not show the near-expiry banner well inside the TTL window', () => {
    const now = Date.now();
    const approval = genericApproval({
      created_at: new Date(now - 10_000).toISOString(),
      ttl_seconds: 300,
      expires_at: Math.floor(now / 1000) + 290,
    });
    renderWithProviders(<ApprovalDetailPanel approval={approval} onApprove={vi.fn()} onReject={vi.fn()} />);
    expect(screen.queryByText(/Expiring soon/)).not.toBeInTheDocument();
  });

  it('shows the near-expiry banner once the last third of the TTL window is reached', () => {
    const now = Date.now();
    const approval = genericApproval({
      created_at: new Date(now - 280_000).toISOString(),
      ttl_seconds: 300,
      expires_at: Math.floor(now / 1000) + 20,
    });
    renderWithProviders(<ApprovalDetailPanel approval={approval} onApprove={vi.fn()} onReject={vi.fn()} />);
    // The banner explains that timing out counts as an automatic rejection.
    expect(screen.getByText(/Expiring soon/)).toBeInTheDocument();
    expect(screen.getByText(/auto-rejected/)).toBeInTheDocument();
  });
});

// ── L7: Discovery approvals get a label + a plain-language spec summary ──
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';
import { parseDiscoveryApproval } from './discovery-approval-payload';

const DISCOVERY_PAYLOAD = {
  task_id: 't-1',
  run_id: 'r-1',
  creator_id: 'user:0000',
  request: {
    spec: {
      approved_root_id: 'root-1',
      evaluator: 'parser_score',
      runtime: 'claude',
      model: 'claude-haiku-4-5',
      branch_count: 2,
      refine_count: 1,
      max_parallelism: 1,
      direction: 'max',
      budget: { max_agent_calls: 2, max_usd: 0.5, max_wall_secs: 300, max_rounds: 1 },
    },
    scorer_hash: 'abc',
    policy_id: 'baseline-parallel-refine',
  },
};

function discoveryApproval(payload: unknown): ApprovalItem {
  return genericApproval({ kind: 'discovery', summary: 'Discovery run on root-1', payload });
}

describe('<ApprovalDetailPanel> discovery (L7)', () => {
  it('shows the discovery label and a readable spec summary', () => {
    renderWithProviders(
      <ApprovalDetailPanel approval={discoveryApproval(DISCOVERY_PAYLOAD)} onApprove={vi.fn()} onReject={vi.fn()} />,
    );
    expect(screen.getByText('Start a code discovery run')).toBeInTheDocument();
    expect(screen.queryByText("Perform an action that hasn't been categorized")).toBeNull();
    // Runtime ids render as product names; the raw id stays out of the card.
    expect(screen.getByText('Claude')).toBeInTheDocument();
    expect(screen.queryByText('claude')).toBeNull();
    expect(screen.getByText('claude-haiku-4-5')).toBeInTheDocument();
    expect(screen.getByText('parser_score')).toBeInTheDocument();
    expect(screen.getByText('Refinements per branch')).toBeInTheDocument();
    expect(screen.getByText('US$0.5')).toBeInTheDocument();
    expect(screen.getByText('5 min')).toBeInTheDocument();
    // nodes per round = branches × (refinements + 1) = 2 × 2
    expect(screen.getByText(/Up to 4 candidates per round/)).toBeInTheDocument();
    // The raw spot-check expander stays.
    expect(screen.getByRole('button', { name: /Spot-check full details/ })).toBeInTheDocument();
  });

  it('renders payload strings as text, never markup', () => {
    const evil = structuredClone(DISCOVERY_PAYLOAD);
    evil.request.spec.model = '<img src=x onerror=alert(1)>';
    const { container } = renderWithProviders(
      <ApprovalDetailPanel approval={discoveryApproval(evil)} onApprove={vi.fn()} onReject={vi.fn()} />,
    );
    expect(screen.getByText('<img src=x onerror=alert(1)>')).toBeInTheDocument();
    expect(container.querySelector('img[src="x"]')).toBeNull();
  });

  it('survives a malformed payload (label stays, generic rendering, no crash)', () => {
    for (const bad of [null, 'oops', 42, { request: 'x' }, { request: { spec: { runtime: 7, budget: 'n' } } }]) {
      const { unmount } = renderWithProviders(
        <ApprovalDetailPanel approval={discoveryApproval(bad)} onApprove={vi.fn()} onReject={vi.fn()} />,
      );
      expect(screen.getByText('Start a code discovery run')).toBeInTheDocument();
      expect(screen.queryByText('Refinements per branch')).toBeNull();
      unmount();
    }
  });

  it('parser drops wrong-typed fields and keeps the valid ones', () => {
    const parsed = parseDiscoveryApproval({
      request: { spec: { runtime: 'codex', model: 5, branch_count: -1, refine_count: 2, budget: { max_usd: 'x', max_rounds: 3 } } },
    });
    expect(parsed).toEqual({ runtime: 'codex', refineCount: 2, maxRounds: 3 });
    expect(parseDiscoveryApproval({ request: { spec: {} } })).toBeNull();
  });

  it('every new discovery key exists in all three locales', () => {
    const keys = Object.keys(en).filter((k) => k.startsWith('approval.discovery.') || k.endsWith('kind.discovery'));
    expect(keys.length).toBeGreaterThan(5);
    for (const k of keys) {
      expect(zhTW).toHaveProperty([k]);
      expect(jaJP).toHaveProperty([k]);
    }
  });
});

describe('<ApprovalDetailPanel> knowledge conflict held for review', () => {
  const held: ApprovalItem = {
    id: 'apr-k',
    agent_id: 'assistant',
    kind: 'knowledge_quarantine',
    summary:
      '對話中出現一則與現有記憶「王小明」衝突的說法，但它的來源可信度較低（trust: conversation_distill 0.60 < operator 1.00），' +
      '所以沒有取代現有內容。已暫存 1 筆；核准後會以您的權限取代現有內容，拒絕則捨棄。內容摘要：王小明的生日是 3 月 5 日',
    created_at: '2026-10-03T00:00:00Z',
    ttl_seconds: 86400,
    payload: { subject: '王小明', quarantined_ids: ['m1'], promote_on_approve: true },
  };

  it('explains the conflict, shows the topic and new statement, and what each decision does', () => {
    renderWithProviders(
      <ApprovalDetailPanel approval={held} onApprove={() => {}} onReject={() => {}} />,
    );
    expect(screen.getByText('New statement conflicts with memory')).toBeInTheDocument();
    expect(screen.getByText('王小明')).toBeInTheDocument();
    expect(screen.getByText('王小明的生日是 3 月 5 日')).toBeInTheDocument();
    expect(screen.getByText('Approve: the new statement replaces the current memory.')).toBeInTheDocument();
    expect(
      screen.getByText('Reject: the new statement is discarded and the current memory stays.'),
    ).toBeInTheDocument();
    // The server sentence (with its internal detail) is not shown as the summary.
    expect(screen.queryByText(/conversation_distill/)).not.toBeInTheDocument();
  });

  it('a burst quarantine keeps the generic view with its summary', () => {
    renderWithProviders(
      <ApprovalDetailPanel
        approval={{ ...held, summary: 'burst summary', payload: { subject: 'x', quarantined_ids: ['a'] } }}
        onApprove={() => {}}
        onReject={() => {}}
      />,
    );
    expect(screen.getByText('burst summary')).toBeInTheDocument();
    expect(screen.queryByText('New statement conflicts with memory')).not.toBeInTheDocument();
  });
});

describe('<ApprovalDetailPanel> knowledge conflict with current value', () => {
  it('shows the current content and the new statement side by side, without the reason or a chat jump', () => {
    const approval: ApprovalItem = {
      id: 'apr-k2',
      agent_id: 'assistant',
      kind: 'knowledge_quarantine',
      summary:
        '對話中有一則關於「退款政策」的新說法，和系統目前採用、來源更可靠的內容不一致，所以還沒有套用。' +
        '目前內容：「七天內可退」。核准會改用這則新說法取代目前內容；拒絕則捨棄這則新說法。內容摘要：三十天內可退',
      created_at: '2026-10-03T00:00:00Z',
      ttl_seconds: 86400,
      channel: 'telegram',
      channel_link: 'https://t.me/x',
      payload: {
        subject: 'policy:refund',
        quarantined_ids: ['h1'],
        promote_on_approve: true,
        disposition: 'trust_held',
        predicate: 'window',
        snippet: '三十天內可退',
        existing_id: 'e1',
        existing_content: '七天內可退',
        reason: 'trust: conversation_distill 0.60 < operator 1.00',
      },
    };
    renderWithProviders(<ApprovalDetailPanel approval={approval} onApprove={() => {}} onReject={() => {}} />);
    expect(screen.getByText('Current content')).toBeInTheDocument();
    expect(screen.getByText('七天內可退')).toBeInTheDocument();
    expect(screen.getByText('三十天內可退')).toBeInTheDocument();
    expect(screen.getByText('退款政策')).toBeInTheDocument();
    expect(screen.queryByText(/trust:/)).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: /Telegram/ })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Telegram/ })).not.toBeInTheDocument();
  });
});

describe('<ApprovalDetailPanel> full-write conflict card', () => {
  const statement = '退款期限改為三十天。' + '詳細條件'.repeat(150) + ' https://evil.example/x **粗體** <b>html</b>';
  const approval: ApprovalItem = {
    id: 'apr-k3',
    agent_id: 'assistant',
    kind: 'knowledge_quarantine',
    summary: '對話中有一則關於「退款政策」的新說法…內容摘要：短',
    created_at: '2026-10-03T00:00:00Z',
    ttl_seconds: 86400,
    payload: {
      disposition: 'trust_held',
      promote_on_approve: true,
      quarantined_ids: ['h1'],
      subject: 'policy:refund',
      subject_label: '退款政策',
      predicate: 'window',
      snippet: statement,
      new_value: '三十天',
      existing_content: '退款期限為七天。',
      existing_content_truncated: true,
      existing_value: '七天',
      reason: 'trust: a 0.6 < b 1.0',
      claim_digest: 'sha256:zzz',
    },
  };

  it('labels value and statement on both sides, shows the whole statement as plain text', () => {
    const { container } = renderWithProviders(
      <ApprovalDetailPanel approval={approval} onApprove={() => {}} onReject={() => {}} />,
    );
    expect(screen.getByText('Current value')).toBeInTheDocument();
    expect(screen.getByText('七天')).toBeInTheDocument();
    expect(screen.getByText('New value')).toBeInTheDocument();
    expect(screen.getByText('三十天')).toBeInTheDocument();
    expect(screen.getByText('退款期限為七天。')).toBeInTheDocument();
    // Whole statement, exact text — no truncation.
    expect(screen.getByText(statement)).toBeInTheDocument();
    expect(screen.getByText(/only the beginning is shown/)).toBeInTheDocument();
    // Plain text: no link, no bold, no injected element.
    const side = container.querySelector('[data-conflict-side="new"]')!;
    expect(side.querySelector('a, b, strong')).toBeNull();
    // No clamp classes on the written text.
    expect(side.innerHTML).not.toMatch(/line-clamp|truncate/);
    expect(screen.queryByText(/sha256|trust:|policy:refund/)).not.toBeInTheDocument();
  });

  it('shows the server error from a failed decision and keeps the buttons', () => {
    renderWithProviders(
      <ApprovalDetailPanel
        approval={approval}
        onApprove={() => {}}
        onReject={() => {}}
        decideError={new Error('decided, but quarantine side-effect failed: memory db locked')}
      />,
    );
    const alert = screen.getByRole('alert');
    expect(alert.textContent).toContain('The decision did not go through');
    expect(alert.textContent).toContain('memory db locked');
    expect(screen.getByRole('button', { name: 'Approve' })).toBeInTheDocument();
  });
});


describe('typed question', () => {
  it('collects an answer and copies its explicit ID command without approving', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    const onApprove = vi.fn(); const onReject = vi.fn();
    renderWithProviders(<ApprovalDetailPanel approval={genericApproval({ request_kind: 'question', id: 'request-full-id', summary: 'Pick the preferred option' })} onApprove={onApprove} onReject={onReject} />);
    expect(screen.getByText('Pick the preferred option')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^Approve/ })).not.toBeInTheDocument();
    const copy = screen.getByRole('button', { name: 'Copy answer command' });
    expect(copy).toBeDisabled();
    fireEvent.change(screen.getByRole('textbox', { name: 'Your answer' }), { target: { value: 'B' } });
    fireEvent.click(copy);
    expect(writeText).toHaveBeenCalledWith('回答 request-full-id B');
    expect(onApprove).not.toHaveBeenCalled(); expect(onReject).not.toHaveBeenCalled();
  });
});

it('shows persisted question answer without decision controls', () => {
  renderWithProviders(<ApprovalDetailPanel approval={genericApproval({ request_kind: 'question', status: 'answered', answer: 'B' })} onApprove={vi.fn()} onReject={vi.fn()} />);
  expect(screen.getByText('Answered')).toBeInTheDocument(); expect(screen.getByText('B')).toBeInTheDocument();
  expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
});
