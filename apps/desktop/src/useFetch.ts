// One request, its answer, and whichever of the two arrived.
//
// Re-runs when its dependencies change, and drops an answer that arrives after
// they have: switching sessions quickly would otherwise show the previous one's
// policy under the new one's name, which is worse than showing nothing.

import { type DependencyList, useEffect, useState } from "react";

import { messageOf } from "./api";

export function useFetch<T>(fetcher: () => Promise<T>, deps: DependencyList) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setData(null);
    setError(null);

    fetcher()
      .then((value) => live && setData(value))
      .catch((e) => live && setError(messageOf(e)));

    return () => {
      live = false;
    };
    // The fetcher closes over `deps`; listing it as well would re-run on every
    // render, since it is a new closure each time.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);

  return { data, error };
}
