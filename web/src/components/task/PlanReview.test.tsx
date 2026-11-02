import { describe, it, expect } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import zhTW from '@/i18n/zh-TW.json';
import { PlanReview } from './PlanReview';

const lenses = [
  { lens: 'restate', status: 'completed', text: '<img src=x onerror=alert(1)>\n- Need a source' },
  { lens: 'simplify', status: 'not_run', text: 'transport error must not be displayed' },
  { lens: 'ready', status: 'completed', text: '- Need permission' },
];

function show(review?: string | null) {
  return render(
    <IntlProvider messages={zhTW} locale="zh-TW">
      <PlanReview review={review} />
    </IntlProvider>,
  );
}

describe('PlanReview', () => {
  it('starts collapsed, shows each lens, and treats review output as plain text', () => {
    const { container } = show(JSON.stringify({ version: 1, lenses }));
    const details = container.querySelector('details');
    expect(details).not.toHaveAttribute('open');
    fireEvent.click(screen.getByText('獨立審查意見（未看過對話）'));
    expect(details).toHaveAttribute('open');
    expect(screen.getByText('重述與釐清')).toBeInTheDocument();
    expect(screen.getByText('簡化')).toBeInTheDocument();
    expect(screen.getByText('執行準備')).toBeInTheDocument();
    expect(screen.getByText('未執行')).toBeInTheDocument();
    expect(container.querySelector('img')).toBeNull();
    expect(container.textContent).toContain(lenses[0].text);
    expect(container.textContent).not.toContain('transport error');
  });

  it('reports every failed lens as not run', () => {
    show(JSON.stringify({ version: 1, lenses: lenses.map(lens => ({ ...lens, status: 'not_run', text: '' })) }));
    expect(screen.getAllByText('未執行')).toHaveLength(3);
  });

  it.each([undefined, null, '', 'bad json', 'null', '{}', '{"version":2,"lenses":[]}',
    JSON.stringify({ version: 1, lenses: [lenses[0], lenses[0], lenses[2]] }),
    JSON.stringify({ version: 1, lenses: [{ ...lenses[0], text: {} }, ...lenses.slice(1)] }),
  ])('renders nothing for absent or invalid data: %s', review => {
    const { container } = show(review);
    expect(container).toBeEmptyDOMElement();
  });
});
