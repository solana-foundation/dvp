<script lang="ts">
	/**
	 * DvP-shaped backdrop: across the canvas, settlement events play out — two
	 * legs (asset in blue, cash in green) converge into a dot, the dot flashes as
	 * it settles, then the two legs cross and leave (asset out one side, cash the
	 * other). Purely decorative; evokes delivery-versus-payment.
	 */
	let canvas: HTMLCanvasElement;

	const ASSET = '#5bb8ff';
	const CASH = '#38e08a';
	const FLASH = '#e6fff2';

	$effect(() => {
		const ctx = canvas.getContext('2d');
		if (!ctx) return;
		const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;

		let raf = 0;
		let w = 0;
		let h = 0;
		type Ev = { cx: number; cy: number; reach: number; tilt: number; dur: number; t0: number };
		let events: Ev[] = [];

		const rand = (a: number, b: number) => a + Math.random() * (b - a);
		const ease = (t: number) => t * t * (3 - 2 * t);

		function mk(now: number): Ev {
			return {
				cx: rand(0.06, 0.94) * w,
				cy: rand(0.14, 0.86) * h,
				reach: rand(70, 150),
				tilt: rand(-0.3, 0.3),
				dur: rand(3400, 5600),
				t0: now - Math.random() * 4000
			};
		}

		function size() {
			const dpr = Math.min(window.devicePixelRatio || 1, 2);
			w = canvas.offsetWidth;
			h = canvas.offsetHeight;
			canvas.width = w * dpr;
			canvas.height = h * dpr;
			ctx!.setTransform(dpr, 0, 0, dpr, 0, 0);
			const n = Math.max(4, Math.floor(w / 210));
			const now = performance.now();
			events = Array.from({ length: n }, () => mk(now));
		}

		function comet(
			x0: number,
			y0: number,
			x1: number,
			y1: number,
			color: string,
			alpha: number
		) {
			const g = ctx!.createLinearGradient(x0, y0, x1, y1);
			g.addColorStop(0, 'transparent');
			g.addColorStop(1, color);
			ctx!.strokeStyle = g;
			ctx!.globalAlpha = alpha;
			ctx!.lineWidth = 1.6;
			ctx!.lineCap = 'round';
			ctx!.beginPath();
			ctx!.moveTo(x0, y0);
			ctx!.lineTo(x1, y1);
			ctx!.stroke();
			ctx!.fillStyle = color;
			ctx!.beginPath();
			ctx!.arc(x1, y1, 1.9, 0, 7);
			ctx!.fill();
		}

		function frame(now: number) {
			ctx!.clearRect(0, 0, w, h);
			for (const e of events) {
				let p = (now - e.t0) / e.dur;
				if (p >= 1) {
					Object.assign(e, mk(now));
					p = 0;
				}
				const dx = Math.cos(e.tilt) * e.reach;
				const dy = Math.sin(e.tilt) * e.reach;
				const lx = e.cx - dx;
				const ly = e.cy - dy;
				const rx = e.cx + dx;
				const ry = e.cy + dy;
				const fade = p < 0.06 ? p / 0.06 : p > 0.9 ? (1 - p) / 0.1 : 1;

				if (p < 0.42) {
					const q = ease(p / 0.42);
					comet(lx, ly, lx + (e.cx - lx) * q, ly + (e.cy - ly) * q, ASSET, 0.4 * fade);
					comet(rx, ry, rx + (e.cx - rx) * q, ry + (e.cy - ry) * q, CASH, 0.4 * fade);
				} else if (p < 0.56) {
					const s = (p - 0.42) / 0.14;
					ctx!.globalAlpha = (1 - s) * 0.55 * fade;
					ctx!.fillStyle = FLASH;
					ctx!.shadowColor = FLASH;
					ctx!.shadowBlur = 14;
					ctx!.beginPath();
					ctx!.arc(e.cx, e.cy, 3, 0, 7);
					ctx!.fill();
					ctx!.shadowBlur = 0;
					ctx!.strokeStyle = FLASH;
					ctx!.globalAlpha = (1 - s) * 0.35 * fade;
					ctx!.lineWidth = 1;
					ctx!.beginPath();
					ctx!.arc(e.cx, e.cy, 3 + s * 11, 0, 7);
					ctx!.stroke();
				} else {
					const q = ease((p - 0.56) / 0.44);
					// crossed: cash leaves left, asset leaves right
					comet(e.cx, e.cy, e.cx + (lx - e.cx) * q, e.cy + (ly - e.cy) * q, CASH, 0.38 * fade);
					comet(e.cx, e.cy, e.cx + (rx - e.cx) * q, e.cy + (ry - e.cy) * q, ASSET, 0.38 * fade);
				}
				ctx!.globalAlpha = 0.18 * fade;
				ctx!.fillStyle = '#8a8a95';
				ctx!.beginPath();
				ctx!.arc(e.cx, e.cy, 1.2, 0, 7);
				ctx!.fill();
			}
			ctx!.globalAlpha = 1;
			raf = requestAnimationFrame(frame);
		}

		size();
		if (reduce) {
			frame(performance.now());
			cancelAnimationFrame(raf);
		} else {
			raf = requestAnimationFrame(frame);
		}
		const onResize = () => size();
		window.addEventListener('resize', onResize);
		return () => {
			cancelAnimationFrame(raf);
			window.removeEventListener('resize', onResize);
		};
	});
</script>

<canvas bind:this={canvas} aria-hidden="true"></canvas>

<style>
	canvas {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		pointer-events: none;
		z-index: 0;
	}
</style>
