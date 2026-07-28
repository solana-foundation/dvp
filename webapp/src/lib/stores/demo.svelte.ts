import { address, type Address, type KeyPairSigner, type Rpc, type SolanaRpcApi } from '@solana/kit';
import { findAssociatedTokenPda, TOKEN_PROGRAM_ADDRESS } from '@solana-program/token';
import { base } from '$app/paths';
import {
	ASSET_TOKEN,
	CASH_TOKEN,
	PRESET,
	ROLES,
	type RoleKey
} from '$lib/config';
import { loadOrCreateSeeds, resetSeeds, signersFromSeeds } from '$lib/solana/roles';
import { makeRpc } from '$lib/solana/rpc';
import {
	createDvp,
	fundLeg,
	settle,
	reject,
	cancel,
	reclaim,
	readTradeState,
	setSponsor,
	type TradeTerms
} from '$lib/solana/dvp';
import type { DvpAddresses } from '$lib/solana/pdas';

export interface LogEntry {
	id: number;
	label: string;
	signature: string;
	ts: number;
}

export interface Holdings {
	asset: bigint;
	cash: bigint;
}

type Closed = 'settle' | 'reject' | 'cancel' | null;

export interface TradeInfo {
	terms: TradeTerms;
	addresses: DvpAddresses;
	createdAt: number;
	closedBy: Closed;
}

class DemoStore {
	// Non-reactive runtime handles.
	private signers: Record<RoleKey, KeyPairSigner> | null = null;
	private rpc: Rpc<SolanaRpcApi> | null = null;
	private logId = 0;

	ready = $state(false);
	error = $state<string | null>(null);
	busy = $state<string | null>(null);
	now = $state(Math.floor(Date.now() / 1000));

	activeRole = $state<RoleKey>('maker');
	addresses = $state<Record<RoleKey, string>>({
		maker: '',
		partyA: '',
		partyB: '',
		authority: ''
	});
	mints = $state<{ asset: string; cash: string } | null>(null);
	started = $state(false);

	balances = $state<{ partyA: Holdings; partyB: Holdings; sol: Record<RoleKey, number> }>({
		partyA: { asset: 0n, cash: 0n },
		partyB: { asset: 0n, cash: 0n },
		sol: { maker: 0, partyA: 0, partyB: 0, authority: 0 }
	});

	trade = $state<TradeInfo | null>(null);
	escrowA = $state(0n);
	escrowB = $state(0n);
	log = $state<LogEntry[]>([]);

	// Derived flags.
	fundedA = $derived(this.trade ? this.escrowA >= this.trade.terms.amountA : false);
	fundedB = $derived(this.trade ? this.escrowB >= this.trade.terms.amountB : false);
	closed = $derived(this.trade?.closedBy != null);
	step = $derived.by<'create' | 'fundA' | 'fundB' | 'settle' | 'done'>(() => {
		if (!this.trade) return 'create';
		if (this.trade.closedBy) return 'done';
		if (!this.fundedA) return 'fundA';
		if (!this.fundedB) return 'fundB';
		return 'settle';
	});
	expiresIn = $derived(
		this.trade ? Number(this.trade.terms.expiryTimestamp) - this.now : 0
	);

	private rpcOrThrow(): Rpc<SolanaRpcApi> {
		if (!this.rpc) this.rpc = makeRpc();
		return this.rpc;
	}

	private signerFor(role: RoleKey): KeyPairSigner {
		if (!this.signers) throw new Error('Signers not initialised');
		return this.signers[role];
	}

	private pushLog(label: string, signature: string) {
		this.log = [{ id: this.logId++, label, signature, ts: Date.now() }, ...this.log];
	}

	private async run<T>(label: string, fn: () => Promise<T>): Promise<T | undefined> {
		this.busy = label;
		this.error = null;
		try {
			const out = await fn();
			return out;
		} catch (e) {
			this.error = e instanceof Error ? e.message : String(e);
			return undefined;
		} finally {
			this.busy = null;
		}
	}

	async init() {
		if (this.ready) return;
		const seeds = loadOrCreateSeeds();
		this.signers = await signersFromSeeds(seeds);
		this.addresses = {
			maker: this.signers.maker.address,
			partyA: this.signers.partyA.address,
			partyB: this.signers.partyB.address,
			authority: this.signers.authority.address
		};
		const res = await fetch(`${base}/api/config`);
		const cfg = await res.json();
		this.mints = cfg.mints;
		if (cfg.treasury) setSponsor(cfg.treasury);
		this.ready = true;
		setInterval(() => (this.now = Math.floor(Date.now() / 1000)), 1000);
		await this.refresh();
	}

	setRole(role: RoleKey) {
		this.activeRole = role;
	}

	/** Start demo: ensure mints exist, drip SOL to every role, mint tokens to A & B. */
	async start() {
		await this.run('Setting up demo wallets', async () => {
			const res = await fetch(`${base}/api/fund`, {
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ addresses: this.addresses })
			});
			if (!res.ok) throw new Error((await res.json()).error ?? 'Faucet failed');
			const body = await res.json();
			this.mints = body.mints;
			this.started = true;
			await this.refresh();
		});
	}

	async createTrade(opts?: {
		amountA?: bigint;
		amountB?: bigint;
		expirySeconds?: number;
		ref?: string;
	}) {
		if (!this.mints) return;
		await this.run('Creating the DvP', async () => {
			const nonce = crypto.getRandomValues(new BigUint64Array(1))[0];
			const expirySeconds = opts?.expirySeconds ?? PRESET.expirySeconds;
			const terms: TradeTerms = {
				settlementAuthority: address(this.addresses.authority),
				userA: address(this.addresses.partyA),
				userB: address(this.addresses.partyB),
				mintA: address(this.mints!.asset),
				mintB: address(this.mints!.cash),
				amountA: opts?.amountA ?? PRESET.amountA,
				amountB: opts?.amountB ?? PRESET.amountB,
				decimalsA: ASSET_TOKEN.decimals,
				decimalsB: CASH_TOKEN.decimals,
				nonce,
				expiryTimestamp: BigInt(Math.floor(Date.now() / 1000) + expirySeconds),
				ref: (opts?.ref && opts.ref.trim()) || 'DEMO-' + nonce.toString().slice(-6)
			};
			const { signature, addresses } = await createDvp(this.rpcOrThrow(), terms);
			this.trade = { terms, addresses, createdAt: Date.now(), closedBy: null };
			this.pushLog('Create DvP', signature);
			await this.refresh();
		});
	}

	/** Clear the finished trade and step back to Create, keeping the same wallets. */
	async newTrade() {
		this.trade = null;
		this.escrowA = 0n;
		this.escrowB = 0n;
		this.error = null;
		this.activeRole = 'maker';
		await this.refresh();
	}

	async fund(leg: 'A' | 'B') {
		if (!this.trade) return;
		const t = this.trade.terms;
		await this.run(`Funding ${leg === 'A' ? 'asset' : 'cash'} leg`, async () => {
			const signature = await fundLeg(
				this.rpcOrThrow(),
				this.signerFor(leg === 'A' ? 'partyA' : 'partyB'),
				leg === 'A'
					? { mint: t.mintA, decimals: t.decimalsA, escrow: this.trade!.addresses.escrowA, amount: t.amountA }
					: { mint: t.mintB, decimals: t.decimalsB, escrow: this.trade!.addresses.escrowB, amount: t.amountB }
			);
			this.pushLog(`Fund ${leg === 'A' ? ASSET_TOKEN.symbol : CASH_TOKEN.symbol} leg`, signature);
			await this.refresh();
		});
	}

	async settleTrade() {
		if (!this.trade) return;
		await this.run('Settling atomically', async () => {
			const signature = await settle(
				this.rpcOrThrow(),
				this.signerFor('authority'),
				this.trade!.terms,
				this.trade!.addresses
			);
			this.trade!.closedBy = 'settle';
			this.pushLog('Settle (atomic swap)', signature);
			await this.refresh();
		});
	}

	async rejectTrade() {
		if (!this.trade) return;
		await this.run('Rejecting trade', async () => {
			const signature = await reject(
				this.rpcOrThrow(),
				this.signerFor(this.activeRole === 'partyB' ? 'partyB' : 'partyA'),
				this.trade!.terms,
				this.trade!.addresses
			);
			this.trade!.closedBy = 'reject';
			this.pushLog('Reject (refund all)', signature);
			await this.refresh();
		});
	}

	async cancelTrade() {
		if (!this.trade) return;
		await this.run('Cancelling trade', async () => {
			const signature = await cancel(
				this.rpcOrThrow(),
				this.signerFor('authority'),
				this.trade!.terms,
				this.trade!.addresses
			);
			this.trade!.closedBy = 'cancel';
			this.pushLog('Cancel (refund all)', signature);
			await this.refresh();
		});
	}

	async reclaimLeg(leg: 'A' | 'B') {
		if (!this.trade) return;
		const t = this.trade.terms;
		await this.run(`Reclaiming ${leg} leg`, async () => {
			const signature = await reclaim(this.rpcOrThrow(), this.signerFor(leg === 'A' ? 'partyA' : 'partyB'), {
				swapDvp: this.trade!.addresses.swapDvp,
				mint: leg === 'A' ? t.mintA : t.mintB,
				escrow: leg === 'A' ? this.trade!.addresses.escrowA : this.trade!.addresses.escrowB
			});
			this.pushLog(`Reclaim ${leg === 'A' ? ASSET_TOKEN.symbol : CASH_TOKEN.symbol} leg`, signature);
			await this.refresh();
		});
	}

	reset() {
		resetSeeds();
		this.trade = null;
		this.started = false;
		this.log = [];
		this.error = null;
		location.reload();
	}

	async refresh() {
		if (!this.mints) return;
		const rpc = this.rpcOrThrow();
		const asset = address(this.mints.asset);
		const cash = address(this.mints.cash);
		const [pa, pb, sol, tradeState] = await Promise.all([
			this.holdings(rpc, address(this.addresses.partyA), asset, cash),
			this.holdings(rpc, address(this.addresses.partyB), asset, cash),
			this.solBalances(rpc),
			this.trade ? readTradeState(rpc, this.trade.addresses) : Promise.resolve(null)
		]);
		this.balances = { partyA: pa, partyB: pb, sol };
		if (tradeState) {
			this.escrowA = tradeState.escrowABalance;
			this.escrowB = tradeState.escrowBBalance;
		}
	}

	private async holdings(
		rpc: Rpc<SolanaRpcApi>,
		owner: Address,
		asset: Address,
		cash: Address
	): Promise<Holdings> {
		const [assetAta] = await findAssociatedTokenPda({ owner, mint: asset, tokenProgram: TOKEN_PROGRAM_ADDRESS });
		const [cashAta] = await findAssociatedTokenPda({ owner, mint: cash, tokenProgram: TOKEN_PROGRAM_ADDRESS });
		const [a, c] = await Promise.all([this.tokenBal(rpc, assetAta), this.tokenBal(rpc, cashAta)]);
		return { asset: a, cash: c };
	}

	private async tokenBal(rpc: Rpc<SolanaRpcApi>, ata: Address): Promise<bigint> {
		try {
			const { value } = await rpc.getTokenAccountBalance(ata).send();
			return BigInt(value.amount);
		} catch {
			return 0n;
		}
	}

	private async solBalances(rpc: Rpc<SolanaRpcApi>): Promise<Record<RoleKey, number>> {
		const entries = await Promise.all(
			ROLES.map(async (r) => {
				try {
					const { value } = await rpc.getBalance(address(this.addresses[r.key])).send();
					return [r.key, Number(value) / 1e9] as const;
				} catch {
					return [r.key, 0] as const;
				}
			})
		);
		return Object.fromEntries(entries) as Record<RoleKey, number>;
	}
}

export const demo = new DemoStore();
