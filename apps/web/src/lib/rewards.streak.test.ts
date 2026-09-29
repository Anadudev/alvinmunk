import { describe, it, expect, vi, beforeEach } from 'vitest';

const readContractMock = vi.fn();
const invokeAndWaitMock = vi.fn();

vi.mock('./contracts', () => ({
  readContract: (...a: unknown[]) => readContractMock(...a),
  invokeAndWait: (...a: unknown[]) => invokeAndWaitMock(...a),
  rewardsId: () => 'CREWARDS',
  args: { u32: (n: number) => ({ __u32: n }), addr: (a: string) => ({ __addr: a }) },
}));

import { getRewardMinStreak, setRewardMinStreak } from './rewards';

describe('streak-gated rewards client', () => {
  beforeEach(() => {
    readContractMock.mockReset();
    invokeAndWaitMock.mockReset();
  });

  it('reads get_reward_min_streak for the reward id', async () => {
    readContractMock.mockResolvedValueOnce(4);
    await expect(getRewardMinStreak(1, 'GSOURCE')).resolves.toBe(4);
    expect(readContractMock).toHaveBeenCalledWith('CREWARDS', 'get_reward_min_streak', [{ __u32: 1 }], 'GSOURCE');
  });

  it('treats a missing result as 0 (no streak required)', async () => {
    readContractMock.mockResolvedValueOnce(null);
    await expect(getRewardMinStreak(1, 'GSOURCE')).resolves.toBe(0);
  });

  it('calls set_reward_min_streak via invokeAndWait', async () => {
    invokeAndWaitMock.mockResolvedValueOnce(undefined);
    const mockWallet = { address: 'GADMIN' } as any;
    await setRewardMinStreak(mockWallet, 1, 4);
    expect(invokeAndWaitMock).toHaveBeenCalledWith(
      'CREWARDS',
      'set_reward_min_streak',
      [{ __u32: 1 }, { __u32: 4 }],
      mockWallet,
    );
  });
});
