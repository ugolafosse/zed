import { expect, test } from "bun:test";
import { mkdtemp, mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

type RotationReceipt = {
	opened: string | null;
};

type RotationEngine = {
	rotationNextReceipt(): RotationReceipt;
};

type RotationEngineConstructor = new (root: string) => RotationEngine;

const irPath = process.env.ZE003_IR_TS;
if (!irPath) throw new Error("ZE003_IR_TS is required");

async function createCollection(name: string): Promise<string> {
	const parent = await mkdtemp(path.join(tmpdir(), `altere-ze003-${name}-`));
	const root = path.join(parent, "collection");
	await mkdir(root);
	await writeFile(
		path.join(root, "one.md"),
		"---\npriority: 10\nreads: 0\n---\n\nOne\n",
	);
	await writeFile(
		path.join(root, "two.md"),
		"---\npriority: 20\nreads: 0\n---\n\nTwo\n",
	);
	return root;
}

async function runFreshProcess(root: string): Promise<RotationReceipt> {
	const child = Bun.spawn(
		[process.execPath, path.join(import.meta.dir, "ze003-driver.ts"), root],
		{
			env: { ...Bun.env, ZE003_IR_TS: irPath },
			stdout: "pipe",
			stderr: "pipe",
		},
	);
	const [exitCode, stdout, stderr] = await Promise.all([
		child.exited,
		new Response(child.stdout).text(),
		new Response(child.stderr).text(),
	]);
	if (exitCode !== 0) throw new Error(stderr.trim());
	return JSON.parse(stdout) as RotationReceipt;
}

test("the TypeScript rotation advances twice while one runtime stays alive", async () => {
	const root = await createCollection("same-process");
	const module = (await import(pathToFileURL(irPath).href)) as {
		TypeScriptIntermittentReading: RotationEngineConstructor;
	};
	const engine = new module.TypeScriptIntermittentReading(root);

	const first = engine.rotationNextReceipt();
	const second = engine.rotationNextReceipt();

	expect(path.basename(first.opened!)).toBe("one.md");
	expect(path.basename(second.opened!)).toBe("two.md");
});

test("the TypeScript rotation preserves advancement across runtime restart", async () => {
	const root = await createCollection("restart");

	const first = await runFreshProcess(root);
	const second = await runFreshProcess(root);

	expect(path.basename(first.opened!)).toBe("one.md");
	expect(path.basename(second.opened!)).toBe("two.md");
});
