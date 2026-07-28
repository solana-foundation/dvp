<script lang="ts">
	/**
	 * Subtle "digital rain" backdrop (payments.org hero motif): faint vertical
	 * streaks in blue/purple/cyan/pink drifting downward. Purely decorative.
	 */
	let canvas: HTMLCanvasElement;

	const COLORS = ['#5b8cff', '#b57bff', '#35d6e6', '#ff77c8', '#38e08a'];

	$effect(() => {
		const ctx = canvas.getContext('2d');
		if (!ctx) return;
		const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;

		let raf = 0;
		let w = 0;
		let h = 0;
		let streaks: {
			x: number;
			y: number;
			len: number;
			speed: number;
			color: string;
			alpha: number;
		}[] = [];

		function seed() {
			const dpr = Math.min(window.devicePixelRatio || 1, 2);
			w = canvas.offsetWidth;
			h = canvas.offsetHeight;
			canvas.width = w * dpr;
			canvas.height = h * dpr;
			ctx!.setTransform(dpr, 0, 0, dpr, 0, 0);
			const count = Math.floor(w / 26);
			streaks = Array.from({ length: count }, () => make());
		}
		function make(fromTop = false) {
			return {
				x: Math.random() * w,
				y: fromTop ? -Math.random() * h : Math.random() * h,
				len: 30 + Math.random() * 120,
				speed: 0.3 + Math.random() * 1.3,
				color: COLORS[(Math.random() * COLORS.length) | 0],
				alpha: 0.05 + Math.random() * 0.22
			};
		}

		function frame() {
			ctx!.clearRect(0, 0, w, h);
			for (const s of streaks) {
				const grad = ctx!.createLinearGradient(s.x, s.y, s.x, s.y + s.len);
				grad.addColorStop(0, 'transparent');
				grad.addColorStop(1, s.color);
				ctx!.globalAlpha = s.alpha;
				ctx!.strokeStyle = grad;
				ctx!.lineWidth = 2;
				ctx!.beginPath();
				ctx!.moveTo(s.x, s.y);
				ctx!.lineTo(s.x, s.y + s.len);
				ctx!.stroke();
				s.y += s.speed;
				if (s.y > h) Object.assign(s, make(true), { y: -s.len });
			}
			ctx!.globalAlpha = 1;
			raf = requestAnimationFrame(frame);
		}

		seed();
		if (reduce) {
			frame();
			cancelAnimationFrame(raf);
			// draw a single static field
			for (const s of streaks) s.speed = 0;
			frame();
			cancelAnimationFrame(raf);
		} else {
			frame();
		}
		const onResize = () => seed();
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
