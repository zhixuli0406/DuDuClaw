import { useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type SkillIndexEntry } from '@/lib/api';
import { Search, Download, Tag, User, ExternalLink } from 'lucide-react';

export function SkillMarketPage() {
  const intl = useIntl();
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<SkillIndexEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [searched, setSearched] = useState(false);

  const handleSearchQuery = async (q: string) => {
    if (!q.trim()) return;
    setLoading(true);
    setSearched(true);
    try {
      const res = await api.skillMarket.search(q);
      setResults(res?.skills ?? []);
    } catch {
      setResults([]);
    } finally {
      setLoading(false);
    }
  };

  const handleSearch = () => handleSearchQuery(query);

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'skills.market.title' })}
        </h2>
        <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'skills.market.subtitle' })}
        </p>
      </div>

      {/* Search bar */}
      <div className="flex gap-3">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-stone-400" />
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleSearch()}
            placeholder={intl.formatMessage({ id: 'skills.market.searchPlaceholder' })}
            className="w-full rounded-lg border border-stone-200 bg-white py-2.5 pl-10 pr-4 text-sm text-stone-900 placeholder-stone-400 transition-colors focus:border-amber-400 focus:outline-none focus:ring-1 focus:ring-amber-400 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-50"
          />
        </div>
        <button
          onClick={handleSearch}
          disabled={loading}
          className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-5 py-2.5 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
        >
          <Search className="h-4 w-4" />
          {intl.formatMessage({ id: 'skills.market.search' })}
        </button>
      </div>

      {/* Results */}
      {loading && (
        <div className="py-12 text-center text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      )}

      {!loading && searched && results.length === 0 && (
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 py-16 dark:border-stone-700">
          <Search className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'skills.market.noResults' })}
          </p>
        </div>
      )}

      {!loading && results.length > 0 && (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {results.map((skill) => (
            <SkillCard key={skill.name} skill={skill} />
          ))}
        </div>
      )}

      {/* Browse by category (static) */}
      {!searched && (
        <div>
          <h3 className="mb-4 text-lg font-medium text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'skills.market.categories' })}
          </h3>
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            {['utility', 'communication', 'code', 'data', 'security', 'ai', 'media', 'automation'].map(
              (cat) => (
                <button
                  key={cat}
                  onClick={() => {
                    setQuery(cat);
                    handleSearchQuery(cat);
                  }}
                  className="flex items-center gap-2 rounded-lg border border-stone-200 bg-white px-4 py-3 text-sm text-stone-700 transition-colors hover:border-amber-300 hover:bg-amber-50 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-300 dark:hover:border-amber-600 dark:hover:bg-amber-900/20"
                >
                  <Tag className="h-4 w-4 text-amber-500" />
                  {cat}
                </button>
              ),
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function SkillCard({ skill }: { skill: SkillIndexEntry }) {
  return (
    <div className="rounded-xl border border-stone-200 bg-white p-5 transition-shadow hover:shadow-md dark:border-stone-800 dark:bg-stone-900">
      <div className="mb-3 flex items-start justify-between">
        <h3 className="font-semibold text-stone-900 dark:text-stone-50">
          {skill.name}
        </h3>
        {skill.url && /^https?:\/\//i.test(skill.url) && (
          <a
            href={skill.url}
            target="_blank"
            rel="noopener noreferrer"
            className="text-stone-400 hover:text-amber-500"
          >
            <ExternalLink className="h-4 w-4" />
          </a>
        )}
      </div>

      <p className="mb-3 text-sm text-stone-600 dark:text-stone-400">
        {skill.description || 'No description'}
      </p>

      {skill.tags.length > 0 && (
        <div className="mb-3 flex flex-wrap gap-1.5">
          {skill.tags.map((tag) => (
            <span
              key={tag}
              className="inline-flex items-center rounded-full bg-stone-100 px-2 py-0.5 text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-400"
            >
              {tag}
            </span>
          ))}
        </div>
      )}

      <div className="flex items-center justify-between border-t border-stone-100 pt-3 dark:border-stone-800">
        {skill.author && (
          <span className="flex items-center gap-1 text-xs text-stone-400">
            <User className="h-3 w-3" />
            {skill.author}
          </span>
        )}
        <button className="inline-flex items-center gap-1 rounded-md bg-amber-100 px-2.5 py-1.5 text-xs font-medium text-amber-700 transition-colors hover:bg-amber-200 dark:bg-amber-900/30 dark:text-amber-400 dark:hover:bg-amber-900/50">
          <Download className="h-3.5 w-3.5" />
          Install
        </button>
      </div>
    </div>
  );
}
