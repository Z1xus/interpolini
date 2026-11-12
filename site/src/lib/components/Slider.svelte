<script lang="ts">
	let {
		label,
		min,
		max,
		step,
		value = $bindable(),
		onchange
	}: {
		label: string;
		min: number;
		max: number;
		step: number;
		value: number;
		onchange?: () => void;
	} = $props();
</script>

<label class="grid grid-cols-[auto_minmax(0,1fr)_4rem] items-center gap-3 text-sm text-muted">
	{label}
	<input
		type="range"
		{min}
		{max}
		{step}
		{value}
		oninput={(event) => {
			value = event.currentTarget.valueAsNumber;
			onchange?.();
		}}
		style:--fill="{((value - min) / (max - min)) * 100}%"
	/>
	<output class="text-end font-mono text-ink">{value}</output>
</label>

<style>
	input {
		appearance: none;
		height: 1rem;
		background: transparent;
		cursor: pointer;

		@media (pointer: coarse) {
			height: 2.5rem;
		}

		&::-webkit-slider-runnable-track {
			height: 2px;
			background: linear-gradient(
				to right,
				var(--color-accent) var(--fill),
				var(--color-seam) var(--fill)
			);
		}

		&::-moz-range-track {
			height: 2px;
			background: var(--color-seam);
		}

		&::-moz-range-progress {
			height: 2px;
			background: var(--color-accent);
		}

		&::-webkit-slider-thumb {
			appearance: none;
			width: 0.5rem;
			height: 1rem;
			margin-block-start: -7px;
			border-radius: 1px;
			background: var(--color-muted);
		}

		&::-moz-range-thumb {
			width: 0.5rem;
			height: 1rem;
			border: 0;
			border-radius: 1px;
			background: var(--color-muted);
		}

		&:hover::-webkit-slider-thumb {
			background: var(--color-ink);
		}

		&:hover::-moz-range-thumb {
			background: var(--color-ink);
		}
	}
</style>
