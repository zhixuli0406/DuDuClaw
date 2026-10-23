import { useCallback, useState } from 'react';
import {
  readSortPreference,
  writeSortPreference,
  type SkillSortKey,
} from '@/lib/skill-library';

/** The viewer's sort for one list, remembered per viewer in local storage. */
export function useSkillSort(scope: string, fallback: SkillSortKey = 'name') {
  const [sort, setSortState] = useState<SkillSortKey>(() => readSortPreference(scope, fallback));
  const setSort = useCallback(
    (key: SkillSortKey) => {
      setSortState(key);
      writeSortPreference(scope, key);
    },
    [scope],
  );
  return [sort, setSort] as const;
}
