/**
 * Ops status snapshot (Green-belt observability — the codeable slice; a full metrics
 * dashboard is infra/later). Reads live on-chain state via simulation (no signing) and
 * prints a one-screen health view: contract liveness, treasury, daily-cap circuit
 * breaker usage, proof-of-funding toggle, and the rank-reward table.
 *
 * Run from repo root:  node scripts/status.mjs
 */
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
const require = createRequire(join(dirname(fileURLToPath(import.meta.url)), '..', 'apps', 'web', 'package.json'));
const { Account, Address, Contract, Keypair, Networks, TransactionBuilder, scValToNative, rpc } = require('@stellar/stellar-sdk');

const RPC = process.env.NEXT_PUBLIC_RPC_URL ?? 'https://soroban-testnet.stellar.org';
const PASSPHRASE = process.env.NEXT_PUBLIC_NETWORK_PASSPHRASE ?? Networks.TESTNET;
const REP = process.env.NEXT_PUBLIC_REPUTATION_CONTRACT_ID ?? 'CBNIZXITUVTRVW6RZGEGCI7KNF46REG4EDM4XUVHKDAV63WOHWW75SZM';
const QUEST = process.env.NEXT_PUBLIC_QUEST_REGISTRY_CONTRACT_ID ?? 'CD6RZUVNQ3TV3X6MNQM25NB2YRFRGMSUGKWTMAIGJOC23C6ESHJKYNFO';
const REWARDS = process.env.NEXT_PUBLIC_REWARDS_CONTRACT_ID ?? 'CBUKGIFOEOS74I2IUUHYNRBZODQFOFCFWIJY3DUJHOUUJV7TT2QYADOU';
const USDC = process.env.NEXT_PUBLIC_USDC_SAC_ID ?? 'CAKT2EK2SFGNXTXVSYZLZXA5YB5QPVHLTVUMRHLJTF5RFFAFMIRNPZT2';
const src = new Account(Keypair.random().publicKey(), '0');
const server = new rpc.Server(RPC);
const usdc = (n) => {
  const num = Number(n);
  if (Number.isNaN(num)) return '0';
  return (num / 1e7).toFixed(7).replace(/0+$/, '').replace(/\.$/, '');
};

async function read(id, method, args = []) {
  try {
    const tx = new TransactionBuilder(src, { fee: '1000000', networkPassphrase: PASSPHRASE })
      .addOperation(new Contract(id).call(method, ...args))
      .setTimeout(30)
      .build();
    const sim = await server.simulateTransaction(tx);
    if (rpc.Api.isSimulationError(sim)) {
      const reason = sim.error ? sim.error.split('\n')[0].trim() || sim.error.trim() : `simulation failed for ${method}`;
      return { ok: false, error: reason };
    }
    const val = sim.result?.retval ? scValToNative(sim.result.retval) : null;
    return { ok: true, data: val };
  } catch (err) {
    return { ok: false, error: err.message ?? String(err) };
  }
}

(async () => {
  let hasError = false;
  const renderError = (err) => {
    hasError = true;
    return `ERROR : ${err}`;
  };

  let ledgerSeq;
  try {
    const latest = await server.getLatestLedger();
    ledgerSeq = latest.sequence;
  } catch (e) {
    ledgerSeq = renderError(e.message ?? String(e));
  }

  console.log('\n📊 Stellar Passport — ops status');
  console.log('   RPC', RPC, '· ledger', ledgerSeq);
  console.log('   contracts: reputation', REP.slice(0, 6), '· quest', QUEST.slice(0, 6), '· rewards', REWARDS.slice(0, 6));

  const [bal, cap, paid, reqFund, week, table] = await Promise.all([
    (async () => {
      try {
        return await read(USDC, 'balance', [new Address(REWARDS).toScVal()]);
      } catch (err) {
        return { ok: false, error: err.message ?? String(err) };
      }
    })(),
    read(REWARDS, 'get_daily_cap'),
    read(REWARDS, 'get_daily_paid'),
    read(REWARDS, 'get_require_funding'),
    read(QUEST, 'get_week'),
    read(REWARDS, 'get_rewards'),
  ]);

  console.log('\n💰 treasury');
  console.log('   USDC balance   ', bal.ok ? `${usdc(bal.data)} USDC` : renderError(bal.error));
  console.log('   daily cap      ', cap.ok ? (Number(cap.data) === 0 ? 'unlimited' : `${usdc(cap.data)} USDC`) : renderError(cap.error));
  console.log('   paid today     ', paid.ok ? `${usdc(paid.data)} USDC` : renderError(paid.error));
  console.log('   proof-of-funding gate', reqFund.ok ? (reqFund.data ? 'ON' : 'off (testnet)') : renderError(reqFund.error));
  console.log('\n🗓  weekly epoch', week.ok ? String(week.data) : renderError(week.error));
  console.log('\n🏅 rank → reward table');
  if (!table.ok) {
    console.log('  ', renderError(table.error));
  } else {
    for (const r of Array.isArray(table.data) ? table.data : []) {
      const id = r.id !== undefined ? `#${r.id}` : '#?';
      const threshold = Number.isNaN(Number(r.threshold)) ? 0 : Number(r.threshold);
      console.log(`   ${id}  ${threshold} XP → ${usdc(r.amount)} USDC  ${r.active ? '' : '(inactive)'}`);
    }
  }
  console.log('');

  if (hasError) {
    process.exit(1);
  }
})().catch((e) => { console.error('FAILED ❌', e.message); process.exit(1); });
