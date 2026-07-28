<script lang="ts">
	import { onMount } from 'svelte';
	import { base } from '$app/paths';
	import RainBackground from '$lib/components/RainBackground.svelte';
	import { demo } from '$lib/stores/demo.svelte';
	import {
		ROLES,
		ASSET_TOKEN,
		CASH_TOKEN,
		PRESET,
		PROGRAM_ID,
		CLUSTER,
		explorerTx,
		type RoleKey
	} from '$lib/config';
	import { formatAmount, shortAddress, formatCountdown } from '$lib/format';

	let { data } = $props();

	onMount(() => {
		demo.init();
	});

	let copied = $state('');
	function copy(text: string, label = '') {
		navigator.clipboard?.writeText(text);
		copied = label || text;
		setTimeout(() => (copied = ''), 1200);
	}

	const roleColor: Record<RoleKey, string> = {
		maker: 'var(--purple)',
		partyA: 'var(--asset)',
		partyB: 'var(--cash)',
		authority: 'var(--amber)'
	};
	const roleLabel = (k: RoleKey) => ROLES.find((r) => r.key === k)!.label;
	const isMaker = () => demo.activeRole === 'maker';

	const shareUrl = $derived(
		demo.trade
			? `${typeof location !== 'undefined' ? location.origin : ''}${base}/ticket?dvp=${demo.trade.addresses.swapDvp}`
			: ''
	);

	const isA = $derived(demo.activeRole === 'partyA');
	const isB = $derived(demo.activeRole === 'partyB');
	const isAuth = $derived(demo.activeRole === 'authority');
	const open = $derived(demo.trade != null && !demo.closed);
	const bothFunded = $derived(demo.fundedA && demo.fundedB);

	const statusText = $derived.by(() => {
		if (!demo.trade) return 'no trade yet';
		if (demo.trade.closedBy === 'settle') return 'settled';
		if (demo.trade.closedBy === 'reject') return 'rejected';
		if (demo.trade.closedBy === 'cancel') return 'cancelled';
		if (demo.fundedA && demo.fundedB) return 'ready to settle';
		if (demo.fundedA) return 'awaiting cash leg';
		if (demo.fundedB) return 'awaiting asset leg';
		return 'awaiting both legs';
	});

	const steps = [
		{ key: 'create', label: 'Create' },
		{ key: 'fundA', label: `Fund ${ASSET_TOKEN.symbol}` },
		{ key: 'fundB', label: `Fund ${CASH_TOKEN.symbol}` },
		{ key: 'settle', label: 'Settle' }
	];
	function stepState(k: string): 'done' | 'active' | 'todo' {
		const order = ['create', 'fundA', 'fundB', 'settle', 'done'];
		if (demo.closed) return 'done';
		if (k === 'fundA' && demo.fundedA) return 'done';
		if (k === 'fundB' && demo.fundedB) return 'done';
		if (order.indexOf(k) < order.indexOf(demo.step)) return 'done';
		if (k === demo.step) return 'active';
		return 'todo';
	}
</script>

<div class="page">
	<header class="topbar">
		<div class="brand">
			<span class="logo">◈</span>
			<span>Solana <b>DvP</b></span>
		</div>
		<div class="topbar-right">
			<span class="pill"><span class="dot live"></span>{CLUSTER}</span>
			<button class="pill mono" onclick={() => copy(PROGRAM_ID, 'program id')} title="Copy program ID">
				{shortAddress(PROGRAM_ID, 6, 6)}
			</button>
			{#if demo.started}
				<button class="btn btn-ghost btn-sm" onclick={() => demo.reset()}>Reset demo</button>
			{/if}
		</div>
	</header>

	<section class="hero">
		<RainBackground />
		<div class="hero-inner">
			<p class="eyebrow">Live demo · Atomic settlement</p>
			<h1>Delivery versus Payment,<br />settled atomically.</h1>
			<p class="hero-sub">
				Two parties swap an asset for cash in a single transaction — no counterparty risk, no
				custodian, no intermediary. Counterparties fund their side with an ordinary token transfer,
				so there's nothing to integrate.
			</p>
			<div class="stats">
				<div class="stat">
					<div class="stat-val num">1</div>
					<div class="stat-label">transaction settles both legs</div>
				</div>
				<div class="stat">
					<div class="stat-val num">0</div>
					<div class="stat-label">counterparty risk</div>
				</div>
				<div class="stat">
					<div class="stat-val num">0</div>
					<div class="stat-label">integration for counterparties</div>
				</div>
			</div>
		</div>
	</section>

	<section class="how">
		<div class="how-step">
			<span class="how-num mono">01</span>
			<b>Maker defines the trade</b>
			<span class="muted">Who delivers what, to whom, and who may settle. Permissionless.</span>
		</div>
		<div class="how-arrow">→</div>
		<div class="how-step">
			<span class="how-num mono">02</span>
			<b>Each side funds an address</b>
			<span class="muted">A plain token transfer to an escrow address. No program call, no integration.</span>
		</div>
		<div class="how-arrow">→</div>
		<div class="how-step">
			<span class="how-num mono">03</span>
			<b>Authority settles atomically</b>
			<span class="muted">Both legs cross in one transaction, or the trade is aborted and refunded.</span>
		</div>
	</section>

	<section class="console">
		{#if !demo.ready}
			<div class="loading muted">Preparing demo…</div>
		{:else}
			<div class="roles">
				{#each ROLES as r (r.key)}
					<button
						class="role"
						class:active={demo.activeRole === r.key}
						style="--role-color:{roleColor[r.key]}"
						onclick={() => demo.setRole(r.key)}
					>
						<span class="role-dot"></span>
						<span class="role-label">{r.label}</span>
						<span class="role-addr mono">{shortAddress(demo.addresses[r.key])}</span>
					</button>
				{/each}
			</div>

			<div class="acting">
				You are acting as
				<b style="color:{roleColor[demo.activeRole]}">{roleLabel(demo.activeRole)}</b>
				<span class="faint mono">· {demo.balances.sol[demo.activeRole].toFixed(3)} SOL</span>
			</div>

			{#if !demo.started}
				<div class="start-card card">
					<h2>Set the stage</h2>
					<p class="muted">
						We'll spin up four demo wallets, drip devnet SOL for fees, and mint the demo tokens:
						<b style="color:var(--asset)">{ASSET_TOKEN.symbol}</b> to Party A and
						<b style="color:var(--cash)">{CASH_TOKEN.symbol}</b> to Party B.
					</p>
					<button class="btn btn-primary" onclick={() => demo.start()} disabled={!!demo.busy}>
						{demo.busy ?? 'Start demo'}
					</button>
				</div>
			{:else}
				<div class="stepper">
					{#each steps as s, i (s.key)}
						<div class="step {stepState(s.key)}">
							<span class="step-node">{stepState(s.key) === 'done' ? '✓' : i + 1}</span>
							<span class="step-label">{s.label}</span>
						</div>
						{#if i < steps.length - 1}<span class="step-line {stepState(s.key)}"></span>{/if}
					{/each}
				</div>

				{#if !demo.trade}
					<div class="create-card card">
						<div>
							<p class="eyebrow">Step 1 — Maker</p>
							<h2>Create the trade ticket</h2>
							<p class="muted">
								Party A will deliver
								<b style="color:var(--asset)"
									>{formatAmount(PRESET.amountA, ASSET_TOKEN.decimals, { compact: true })}
									{ASSET_TOKEN.symbol}</b
								>
								for Party B's
								<b style="color:var(--cash)"
									>{formatAmount(PRESET.amountB, CASH_TOKEN.decimals, { compact: true })}
									{CASH_TOKEN.symbol}</b
								>. The Settlement Authority is the only one who can settle.
							</p>
						</div>
						<button
							class="btn btn-primary"
							onclick={() => demo.createTrade()}
							disabled={!!demo.busy || !isMaker()}
						>
							{demo.busy ?? 'Create DvP'}
						</button>
						{#if !isMaker()}<p class="hint">Switch to <b>Maker</b> to create the trade.</p>{/if}
					</div>
				{:else}
					<div class="ticket card">
						<div class="ticket-head">
							<p class="eyebrow">Trade ticket</p>
							<span class="status" class:closed={demo.closed} class:ready={bothFunded && open}>
								{statusText}
							</span>
						</div>

						{@render legCard(
							'A',
							ASSET_TOKEN.symbol,
							demo.trade.terms.amountA,
							ASSET_TOKEN.decimals,
							'var(--asset)',
							'partyA',
							demo.escrowA,
							demo.trade.addresses.escrowA,
							demo.fundedA
						)}
						{@render legCard(
							'B',
							CASH_TOKEN.symbol,
							demo.trade.terms.amountB,
							CASH_TOKEN.decimals,
							'var(--cash)',
							'partyB',
							demo.escrowB,
							demo.trade.addresses.escrowB,
							demo.fundedB
						)}

						<div class="ticket-meta">
							<div>
								<span class="eyebrow">Settlement authority</span>
								{@render addrChip(demo.addresses.authority)}
							</div>
							<div>
								<span class="eyebrow">Expires in</span>
								<span class="mono" class:warn={demo.expiresIn < 120}
									>{formatCountdown(demo.expiresIn)}</span
								>
							</div>
							<div>
								<span class="eyebrow">DvP account</span>
								{@render addrChip(demo.trade.addresses.swapDvp)}
							</div>
						</div>

						{#if open}
							<div class="share">
								<span class="eyebrow">Share with a counterparty</span>
								<div class="share-row">
									<code class="mono">{shortAddress(shareUrl, 30, 8)}</code>
									<button class="btn btn-sm" onclick={() => copy(shareUrl, 'link')}>
										{copied === 'link' ? 'Copied' : 'Copy link'}
									</button>
									<a class="btn btn-sm btn-ghost" href={shareUrl} target="_blank">Open ↗</a>
								</div>
								<p class="faint">
									They verify the terms on-chain and send a normal token transfer — no wallet
									integration with this program.
								</p>
							</div>
						{/if}
					</div>

					<div class="action">
						{#if demo.closed}
							{@render settledBanner()}
						{:else if bothFunded}
							<div class="action-primary">
								<div>
									<b>Both legs are funded.</b>
									<span class="muted">The authority can now settle both sides atomically.</span>
								</div>
								<button
									class="btn btn-primary"
									onclick={() => demo.settleTrade()}
									disabled={!!demo.busy || !isAuth}
								>
									{demo.busy ?? '⇄ Settle atomically'}
								</button>
							</div>
							{#if !isAuth}<p class="hint">Switch to <b>Settlement Authority</b> to settle.</p>{/if}
						{:else}
							<div class="action-primary">
								<span class="muted">Fund each leg above by acting as that party — or abort:</span>
							</div>
						{/if}
						{#if open}
							<div class="abort">
								{#if isAuth}
									<button class="btn btn-danger btn-sm" onclick={() => demo.cancelTrade()} disabled={!!demo.busy}>Cancel (authority)</button>
								{/if}
								{#if isA || isB}
									<button class="btn btn-danger btn-sm" onclick={() => demo.rejectTrade()} disabled={!!demo.busy}>Reject trade</button>
								{/if}
							</div>
						{/if}
					</div>
				{/if}

				<div class="balances">
					{@render partyBalance('partyA', 'Party A · Seller')}
					{@render partyBalance('partyB', 'Party B · Buyer')}
				</div>

				{#if demo.log.length}
					<div class="log card">
						<p class="eyebrow">On-chain activity</p>
						{#each demo.log as e (e.id)}
							<div class="log-row fade-in">
								<span class="log-label">{e.label}</span>
								<a class="mono log-sig" href={explorerTx(e.signature)} target="_blank"
									>{shortAddress(e.signature, 8, 8)} ↗</a
								>
							</div>
						{/each}
					</div>
				{/if}
			{/if}

			{#if demo.error}
				<div class="error card">⚠ {demo.error}</div>
			{/if}
		{/if}
	</section>

	<footer class="foot faint">
		Devnet demo · program <span class="mono">{shortAddress(PROGRAM_ID, 6, 6)}</span>
		{#if data.userEmail}· {data.userEmail}{/if}
	</footer>
</div>

{#snippet addrChip(a: string)}
	<button class="addr mono" onclick={() => copy(a, a)} title="Copy">
		{shortAddress(a, 4, 4)}<span class="addr-copy">{copied === a ? '✓' : '⧉'}</span>
	</button>
{/snippet}

{#snippet legCard(
	leg: 'A' | 'B',
	symbol: string,
	amount: bigint,
	decimals: number,
	color: string,
	party: RoleKey,
	escrowBal: bigint,
	escrowAddr: string,
	funded: boolean
)}
	{@const pct = amount > 0n ? Math.min(100, Number((escrowBal * 100n) / amount)) : 0}
	{@const canFund = open && !funded && demo.activeRole === party}
	{@const canReclaim = open && escrowBal > 0n && demo.activeRole === party}
	<div class="leg" style="--leg:{color}">
		<div class="leg-main">
			<div class="leg-id">
				<span class="leg-tag mono">Leg {leg} · {leg === 'A' ? 'asset' : 'cash'}</span>
				<div class="leg-amt">
					<span class="num">{formatAmount(amount, decimals, { compact: true })}</span>
					<span class="leg-sym">{symbol}</span>
				</div>
				<span class="muted">from <b style="color:{color}">{roleLabel(party)}</b></span>
			</div>
			<div class="leg-fund">
				<div class="escrow-line">
					<span class="eyebrow">Escrow address</span>
					{@render addrChip(escrowAddr)}
				</div>
				<div class="bar"><span style="width:{pct}%; background:{color}"></span></div>
				<div class="bar-label mono">
					{funded
						? '✓ funded'
						: `${formatAmount(escrowBal, decimals, { compact: true })} / ${formatAmount(amount, decimals, { compact: true })}`}
				</div>
				<div class="leg-actions">
					<button
						class="btn btn-sm"
						style={canFund ? `background:${color};color:#04120a;font-weight:600` : ''}
						onclick={() => demo.fund(leg)}
						disabled={!canFund || !!demo.busy}
					>
						Fund my leg
					</button>
					{#if canReclaim}
						<button class="btn btn-sm btn-ghost" onclick={() => demo.reclaimLeg(leg)} disabled={!!demo.busy}>Reclaim</button>
					{/if}
				</div>
				{#if open && !funded && demo.activeRole !== party}
					<p class="hint">Switch to <b style="color:{color}">{roleLabel(party)}</b> to fund.</p>
				{/if}
			</div>
		</div>
	</div>
{/snippet}

{#snippet partyBalance(role: RoleKey, title: string)}
	{@const h = role === 'partyA' ? demo.balances.partyA : demo.balances.partyB}
	<div class="pcard card">
		<div class="pcard-head">
			<span class="avatar" style="background:{roleColor[role]}">{title[6]}</span>
			<div>
				<div class="pcard-name">{title}</div>
				<div class="faint mono">{shortAddress(demo.addresses[role])}</div>
			</div>
		</div>
		<div class="pcard-rows">
			<div class="pcard-row">
				<span class="pcard-sym mono" style="color:var(--asset)">{ASSET_TOKEN.symbol}</span>
				<span class="num pcard-bal">{formatAmount(h.asset, ASSET_TOKEN.decimals, { compact: true })}</span>
			</div>
			<div class="pcard-row">
				<span class="pcard-sym mono" style="color:var(--cash)">{CASH_TOKEN.symbol}</span>
				<span class="num pcard-bal">{formatAmount(h.cash, CASH_TOKEN.decimals, { compact: true })}</span>
			</div>
		</div>
	</div>
{/snippet}

{#snippet settledBanner()}
	<div class="settled {demo.trade?.closedBy}">
		{#if demo.trade?.closedBy === 'settle'}
			<span class="settled-icon">⇄</span>
			<div>
				<b>Settled atomically.</b>
				<span class="muted">Both legs crossed in a single transaction. Check the balances above.</span>
			</div>
		{:else}
			<span class="settled-icon">↩</span>
			<div>
				<b>Trade {demo.trade?.closedBy === 'reject' ? 'rejected' : 'cancelled'}.</b>
				<span class="muted">All funded legs were refunded to their depositors.</span>
			</div>
		{/if}
	</div>
{/snippet}

<style>
	.page {
		max-width: var(--maxw);
		margin: 0 auto;
		padding: 0 1.25rem 5rem;
	}
	.topbar {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: 1.1rem 0;
	}
	.brand {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 1.05rem;
	}
	.brand .logo {
		color: var(--green);
		font-size: 1.1rem;
	}
	.brand b {
		font-weight: 600;
	}
	.topbar-right {
		display: flex;
		align-items: center;
		gap: 0.6rem;
	}
	.topbar-right .pill {
		cursor: pointer;
	}
	.hero {
		position: relative;
		overflow: hidden;
		border-radius: var(--radius);
		border: 1px solid var(--border);
		background: linear-gradient(180deg, #0b0b12, #08080a 70%);
		margin-top: 0.5rem;
	}
	.hero-inner {
		position: relative;
		z-index: 1;
		padding: 4rem 2.5rem 2.5rem;
	}
	.hero h1 {
		font-size: clamp(2.4rem, 6vw, 4.2rem);
		margin: 0.6rem 0 1rem;
	}
	.hero-sub {
		max-width: 40rem;
		color: var(--text-muted);
		font-size: 1.05rem;
	}
	.stats {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 1px;
		margin-top: 2.5rem;
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
		overflow: hidden;
		background: var(--border);
	}
	.stat {
		background: rgba(8, 8, 12, 0.7);
		padding: 1.1rem 1.2rem;
	}
	.stat-val {
		font-size: 2rem;
		color: var(--green);
	}
	.stat-label {
		color: var(--text-muted);
		font-size: 0.9rem;
	}
	.how {
		display: flex;
		align-items: stretch;
		gap: 1rem;
		margin: 1.5rem 0;
	}
	.how-step {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		padding: 1.1rem 1.2rem;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
	}
	.how-num {
		color: var(--text-faint);
		font-size: 0.8rem;
	}
	.how-step b {
		font-weight: 600;
	}
	.how-step .muted {
		font-size: 0.88rem;
	}
	.how-arrow {
		align-self: center;
		color: var(--text-faint);
	}
	.console {
		margin-top: 1.5rem;
	}
	.loading {
		padding: 3rem;
		text-align: center;
	}
	.roles {
		display: grid;
		grid-template-columns: repeat(4, 1fr);
		gap: 0.6rem;
	}
	.role {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.35rem;
		padding: 0.8rem 0.9rem;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
		cursor: pointer;
		transition: border-color 0.15s;
	}
	.role:hover {
		border-color: var(--border-strong);
	}
	.role.active {
		border-color: var(--role-color);
		box-shadow: inset 0 0 0 1px var(--role-color);
	}
	.role-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--role-color);
	}
	.role-label {
		font-weight: 500;
		font-size: 0.92rem;
	}
	.role-addr {
		font-size: 0.75rem;
		color: var(--text-faint);
	}
	.acting {
		margin: 0.9rem 0 1.3rem;
		color: var(--text-muted);
		font-size: 0.95rem;
	}
	.start-card,
	.create-card {
		padding: 1.6rem;
		display: flex;
		flex-direction: column;
		gap: 1rem;
		align-items: flex-start;
	}
	.start-card h2,
	.create-card h2 {
		font-size: 1.6rem;
	}
	.create-card {
		gap: 1.2rem;
	}
	.hint {
		font-size: 0.85rem;
		color: var(--text-faint);
		margin: 0;
	}
	.stepper {
		display: flex;
		align-items: center;
		margin: 0.5rem 0 1.5rem;
	}
	.step {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		color: var(--text-faint);
	}
	.step-node {
		width: 26px;
		height: 26px;
		border-radius: 50%;
		border: 1px solid var(--border-strong);
		display: grid;
		place-items: center;
		font-size: 0.8rem;
		font-family: var(--font-mono);
	}
	.step.active {
		color: var(--text);
	}
	.step.active .step-node {
		border-color: var(--green);
		color: var(--green);
		box-shadow: 0 0 0 3px var(--green-dim);
	}
	.step.done {
		color: var(--text-muted);
	}
	.step.done .step-node {
		background: var(--green);
		border-color: var(--green);
		color: #04120a;
	}
	.step-line {
		flex: 1;
		height: 1px;
		background: var(--border-strong);
		margin: 0 0.7rem;
	}
	.step-line.done {
		background: var(--green);
	}
	.ticket {
		padding: 1.4rem;
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}
	.ticket-head {
		display: flex;
		justify-content: space-between;
		align-items: center;
	}
	.status {
		font-size: 0.85rem;
		color: var(--text-muted);
		padding: 0.25rem 0.7rem;
		border: 1px solid var(--border);
		border-radius: var(--radius-pill);
	}
	.status.ready {
		color: var(--green);
		border-color: var(--green);
	}
	.status.closed {
		color: var(--text-faint);
	}
	.leg {
		border: 1px solid var(--border);
		border-left: 2px solid var(--leg);
		border-radius: var(--radius-sm);
		padding: 1.1rem 1.2rem;
		background: var(--bg-2);
	}
	.leg-main {
		display: grid;
		grid-template-columns: 1fr 1.3fr;
		gap: 1.5rem;
	}
	.leg-tag {
		font-size: 0.72rem;
		color: var(--text-faint);
		text-transform: uppercase;
		letter-spacing: 0.12em;
	}
	.leg-amt {
		display: flex;
		align-items: baseline;
		gap: 0.4rem;
		margin: 0.2rem 0;
	}
	.leg-amt .num {
		font-size: 2rem;
	}
	.leg-sym {
		color: var(--leg);
		font-weight: 600;
	}
	.leg-fund {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}
	.escrow-line {
		display: flex;
		justify-content: space-between;
		align-items: center;
	}
	.bar {
		height: 8px;
		background: rgba(255, 255, 255, 0.06);
		border-radius: 6px;
		overflow: hidden;
	}
	.bar span {
		display: block;
		height: 100%;
		border-radius: 6px;
		transition: width 0.5s ease;
	}
	.bar-label {
		font-size: 0.78rem;
		color: var(--text-muted);
	}
	.leg-actions {
		display: flex;
		gap: 0.5rem;
		margin-top: 0.2rem;
	}
	.ticket-meta {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 1rem;
		padding-top: 0.6rem;
		border-top: 1px solid var(--border);
	}
	.ticket-meta > div {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
	}
	.warn {
		color: var(--amber);
	}
	.share {
		border-top: 1px solid var(--border);
		padding-top: 0.9rem;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}
	.share-row {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}
	.share-row code {
		flex: 1;
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
		padding: 0.5rem 0.7rem;
		font-size: 0.8rem;
		color: var(--text-muted);
		overflow: hidden;
		white-space: nowrap;
	}
	.addr {
		font-size: 0.82rem;
		color: var(--text);
		background: transparent;
		border: none;
		cursor: pointer;
		display: inline-flex;
		gap: 0.3rem;
		align-items: center;
		padding: 0;
	}
	.addr-copy {
		color: var(--text-faint);
		font-size: 0.75rem;
	}
	.action {
		margin-top: 1.2rem;
		display: flex;
		flex-direction: column;
		gap: 0.8rem;
	}
	.action-primary {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 1rem;
		padding: 1.2rem 1.4rem;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--radius);
	}
	.action-primary b {
		display: block;
	}
	.abort {
		display: flex;
		gap: 0.5rem;
		justify-content: flex-end;
	}
	.settled {
		display: flex;
		align-items: center;
		gap: 1rem;
		padding: 1.4rem 1.6rem;
		border-radius: var(--radius);
		border: 1px solid var(--green);
		background: var(--green-dim);
	}
	.settled.reject,
	.settled.cancel {
		border-color: var(--border-strong);
		background: var(--surface);
	}
	.settled-icon {
		font-size: 1.8rem;
		color: var(--green);
	}
	.settled.reject .settled-icon,
	.settled.cancel .settled-icon {
		color: var(--text-muted);
	}
	.settled b {
		display: block;
	}
	.balances {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 1rem;
		margin-top: 1.4rem;
	}
	.pcard {
		padding: 1.3rem;
	}
	.pcard-head {
		display: flex;
		align-items: center;
		gap: 0.7rem;
		margin-bottom: 1rem;
	}
	.avatar {
		width: 34px;
		height: 34px;
		border-radius: 50%;
		display: grid;
		place-items: center;
		color: #04120a;
		font-weight: 700;
	}
	.pcard-name {
		font-weight: 500;
	}
	.pcard-rows {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}
	.pcard-row {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		border-top: 1px solid var(--border);
		padding-top: 0.6rem;
	}
	.pcard-sym {
		font-size: 0.8rem;
		letter-spacing: 0.08em;
	}
	.pcard-bal {
		font-size: 1.5rem;
	}
	.log {
		margin-top: 1.4rem;
		padding: 1.2rem 1.4rem;
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}
	.log-row {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: 0.35rem 0;
		border-bottom: 1px solid var(--border);
	}
	.log-label {
		font-size: 0.92rem;
	}
	.log-sig {
		color: var(--text-muted);
		font-size: 0.8rem;
	}
	.error {
		margin-top: 1.2rem;
		padding: 1rem 1.2rem;
		border-color: rgba(255, 107, 107, 0.4);
		color: var(--red);
		background: rgba(255, 107, 107, 0.06);
	}
	.foot {
		margin-top: 3rem;
		text-align: center;
		font-size: 0.82rem;
	}
	@media (max-width: 760px) {
		.how,
		.stats,
		.roles,
		.ticket-meta,
		.balances,
		.leg-main {
			grid-template-columns: 1fr;
			flex-direction: column;
		}
		.how-arrow {
			display: none;
		}
		.hero-inner {
			padding: 2.5rem 1.4rem 1.8rem;
		}
	}
</style>
