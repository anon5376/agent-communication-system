#!/usr/bin/env node
function argument(name) {
    const index = process.argv.indexOf(name);
    return index >= 0 ? String(process.argv[index + 1] ?? "") : null;
}
function parseArgs() {
    const baseUrl = argument("--base-url") ?? "http://127.0.0.1:1234/v1";
    const model = argument("--model") ?? "";
    const prompt = argument("--prompt") ?? "";
    const apiKeyEnv = argument("--api-key-env") ?? "OPENAI_API_KEY";
    if (!model)
        throw new Error("--model is required");
    if (!prompt)
        throw new Error("--prompt is required");
    const temperatureRaw = argument("--temperature");
    const maxTokensRaw = argument("--max-tokens");
    return {
        baseUrl: baseUrl.replace(/\/$/, ""),
        model,
        prompt,
        apiKeyEnv,
        temperature: temperatureRaw === null ? undefined : Number(temperatureRaw),
        maxTokens: maxTokensRaw === null ? undefined : Number(maxTokensRaw),
    };
}
async function main() {
    if (process.argv.includes("--help") || process.argv.includes("-h")) {
        process.stdout.write("qagent OpenAI-compatible harness\n\n--base-url URL --model MODEL --prompt TEXT [--api-key-env NAME]\n");
        return;
    }
    const args = parseArgs();
    const apiKey = process.env[args.apiKeyEnv] ?? "";
    const headers = { "content-type": "application/json" };
    if (apiKey)
        headers.authorization = `Bearer ${apiKey}`;
    const body = {
        model: args.model,
        messages: [{ role: "user", content: args.prompt }],
        stream: false,
    };
    if (Number.isFinite(args.temperature))
        body.temperature = args.temperature;
    if (Number.isFinite(args.maxTokens))
        body.max_tokens = args.maxTokens;
    const response = await fetch(`${args.baseUrl}/chat/completions`, {
        method: "POST",
        headers,
        body: JSON.stringify(body),
        signal: AbortSignal.timeout(Number(process.env.QAGENT_HTTP_TIMEOUT_MS ?? process.env.AGENT_BUS_HTTP_TIMEOUT_MS ?? 3_600_000)),
    });
    const text = await response.text();
    if (!response.ok) {
        throw new Error(`OpenAI-compatible endpoint returned ${response.status}: ${text.slice(0, 1000)}`);
    }
    const payload = JSON.parse(text);
    const content = payload.choices?.[0]?.message?.content;
    if (typeof content !== "string") {
        throw new Error("OpenAI-compatible response did not contain choices[0].message.content");
    }
    const usage = payload.usage ?? {};
    process.stdout.write(`${JSON.stringify({
        result: content,
        usage: {
            inputTokens: Number(usage.prompt_tokens ?? 0),
            outputTokens: Number(usage.completion_tokens ?? 0),
            totalTokens: Number(usage.total_tokens ?? 0),
            costUSD: 0,
        },
        model: payload.model ?? args.model,
        id: payload.id ?? null,
    })}\n`);
}
main().catch((error) => {
    process.stderr.write(`${error?.stack ?? error?.message ?? error}\n`);
    process.exit(1);
});
export {};
//# sourceMappingURL=openai-compatible-harness.js.map