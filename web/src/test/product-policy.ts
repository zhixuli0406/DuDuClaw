import { useProductPolicyStore } from '@/lib/product-policy';

/** Component tests exercise the ordinary catalog under an explicitly loaded policy. */
export async function setStandardProductPolicyFixture() {
  // Let synchronous mocked connection reloads settle before installing the fixture.
  await Promise.resolve();
  useProductPolicyStore.setState({
    policy: { profile: 'standard', blocked_channels: [], blocked_runtimes: [], blocked_providers: [] },
    loading: false,
    error: false,
  });
}
