<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { base } from '$app/paths';
	import { address, type Address } from '@solana/kit';
	import { TOKEN_PROGRAM_ADDRESS } from '@solana-program/token';
	import { makeRpc } from '$lib/solana/rpc';
	import { verifySwapDvp, findSwapDvpEscrowAta } from '$lib/dvp';
	import { ASSET_TOKEN, CASH_TOKEN, CLUSTER, explorerAddress, type TokenMeta } from '$lib/config';
	import { formatAmount, shortAddress, formatCountdown } from '$lib/format';

	type LegView = {
		party: Address;
		amount: bigint;
		token: TokenMeta;
		escrow: Address;
	};

	let status = $state<'loading' | 'ok' | 'error'>('loading');
	let errorMsg = $state('');
	let dvpAddr = $state('');
	let authority = $state('');
	let expiry = $state(0);
	let now = $state(Math.floor(Date.now() / 1000));
	let legs = $state<LegView[]>([]);
	let copied = $state('');

	function copy(t: string) {
		navigator.clipboard?.writeText(t);
		copied = t;
		setTimeout(() => (copied = ''), 1200);
	}

	function tokenFor(mint: string, mints: { asset: string; cash: string }): TokenMeta {
		if (mint === mints.asset) return ASSET_TOKEN;
		if (mint === mints.cash) return CASH_TOKEN;
		return { key: 'cash', symbol: 'tokens', name: mint, decimals: 0 };
	}

	onMount(async () => {
		try {
			const dvp = page.url.searchParams.get('dvp');
			if (!dvp) throw new Error('No DvP address in the link.');
			dvpAddr = dvp;
			const cfg = await (await fetch(`${base}/api/config`)).json();
			const mints = cfg.mints as { asset: string; cash: string };
			const rpc = makeRpc();
			const account = await verifySwapDvp(rpc, address(dvp));
			const d = account.data;
			authority = d.settlementAuthority;
			expiry = Number(d.expiryTimestamp);
			const [escrowA] = await findSwapDvpEscrowAta({
				swapDvp: address(dvp),
				mint: d.mintA,
				tokenProgram: TOKEN_PROGRAM_ADDRESS
			});
			const [escrowB] = await findSwapDvpEscrowAta({
				swapDvp: address(dvp),
				mint: d.mintB,
				tokenProgram: TOKEN_PROGRAM_ADDRESS
			});
			legs = [
				{ party: d.userA, amount: d.amountA, token: tokenFor(d.mintA, mints), escrow: escrowA },
				{ party: d.userB, amount: d.amountB, token: tokenFor(d.mintB, mints), escrow: escrowB }
			];
			status = 'ok';
			setInterval(() => (now = Math.floor(Date.now() / 1000)), 1000);
		} catch (e) {
			errorMsg = e instanceof Error ? e.message : String(e);
			status = 'error';
		}
	});
</script>

<div class="wrap">
	<header class="top">
		<a class="brand" href="{base}/"><span class="logo">◈</span> Solana <b>DvP</b></a>
		<span class="pill"><span class="dot live"></span>{CLUSTER}</span>
	</header>

	{#if status === 'loading'}
		<div class="card pad muted">Verifying the trade on-chain…</div>
	{:else if status === 'error'}
		<div class="card pad err">⚠ {errorMsg}</div>
	{:else}
		<p class="eyebrow">Settlement instruction</p>
		<h1>You've been asked to settle a trade.</h1>
		<div class="verified">
			<span class="check">✓</span>
			<div>
				<b>Terms verified on-chain.</b>
				<span class="muted"
					>This escrow is a canonical account owned by the DvP program — the amounts and addresses
					below are exactly what's recorded on Solana.</span
				>
			</div>
		</div>

		{#each legs as leg, i (i)}
			<div class="leg card" style="--c:{i === 0 ? 'var(--asset)' : 'var(--cash)'}">
				<div class="leg-top">
					<span class="eyebrow">{i === 0 ? 'Asset leg' : 'Cash leg'}</span>
					<span class="muted mono">{shortAddress(leg.party, 4, 4)} delivers</span>
				</div>
				<div class="amt">
					<span class="num">{formatAmount(leg.amount, leg.token.decimals, { compact: true })}</span>
					<span class="sym">{leg.token.symbol}</span>
				</div>
				<div class="send">
					<span class="eyebrow">Send this amount to</span>
					<div class="send-row">
						<code class="mono">{leg.escrow}</code>
						<button class="btn btn-sm" onclick={() => copy(leg.escrow)}>
							{copied === leg.escrow ? 'Copied' : 'Copy'}
						</button>
					</div>
					<p class="faint">
						A normal {leg.token.symbol} transfer to this address — from any wallet, exchange, or custodian.
						No integration with the DvP program required.
					</p>
				</div>
			</div>
		{/each}

		<div class="meta card pad">
			<div><span class="eyebrow">Settlement authority</span><span class="mono">{shortAddress(authority, 6, 6)}</span></div>
			<div><span class="eyebrow">Expires in</span><span class="mono">{formatCountdown(expiry - now)}</span></div>
			<div>
				<span class="eyebrow">DvP account</span>
				<a class="mono link" href={explorerAddress(dvpAddr)} target="_blank">{shortAddress(dvpAddr, 6, 6)} ↗</a>
			</div>
		</div>
		<p class="foot faint">
			Once both legs are funded, the settlement authority settles both sides in a single atomic
			transaction. Until then, either party can pull their funds back.
		</p>
	{/if}
</div>

<style>
	.wrap {
		max-width: 640px;
		margin: 0 auto;
		padding: 1.25rem 1.25rem 4rem;
	}
	.top {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: 0.6rem 0 2rem;
	}
	.brand {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}
	.brand .logo {
		color: var(--green);
	}
	.brand b {
		font-weight: 600;
	}
	h1 {
		font-size: clamp(1.8rem, 5vw, 2.6rem);
		margin: 0.4rem 0 1.4rem;
	}
	.pad {
		padding: 1.4rem;
	}
	.muted {
		color: var(--text-muted);
	}
	.err {
		color: var(--red);
	}
	.verified {
		display: flex;
		gap: 0.8rem;
		align-items: flex-start;
		padding: 1rem 1.2rem;
		border: 1px solid var(--green);
		background: var(--green-dim);
		border-radius: var(--radius);
		margin-bottom: 1.2rem;
	}
	.verified b {
		display: block;
	}
	.check {
		color: var(--green);
		font-size: 1.3rem;
	}
	.leg {
		border-left: 2px solid var(--c);
		padding: 1.3rem 1.4rem;
		margin-bottom: 1rem;
	}
	.leg-top {
		display: flex;
		justify-content: space-between;
		align-items: center;
	}
	.amt {
		display: flex;
		align-items: baseline;
		gap: 0.5rem;
		margin: 0.4rem 0 1rem;
	}
	.amt .num {
		font-size: 2.6rem;
	}
	.sym {
		color: var(--c);
		font-weight: 600;
		font-size: 1.1rem;
	}
	.send {
		border-top: 1px solid var(--border);
		padding-top: 0.9rem;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}
	.send-row {
		display: flex;
		gap: 0.5rem;
		align-items: center;
	}
	.send-row code {
		flex: 1;
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
		padding: 0.55rem 0.7rem;
		font-size: 0.8rem;
		overflow-x: auto;
		white-space: nowrap;
		color: var(--text);
	}
	.meta {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 1rem;
		margin-top: 0.4rem;
	}
	.meta > div {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
	}
	.link {
		color: var(--text-muted);
	}
	.foot {
		margin-top: 1.4rem;
		font-size: 0.85rem;
	}
	@media (max-width: 620px) {
		.meta {
			grid-template-columns: 1fr;
		}
	}
</style>
