import { env, exports } from "cloudflare:workers";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "./helpers";

afterEach(() => {
  vi.restoreAllMocks();
});

const APP = "http://127.0.0.1:53682/cb";

function b64url(bytes: ArrayBuffer): string {
  let bin = "";
  for (const b of new Uint8Array(bytes)) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

async function challengeFor(verifier: string): Promise<string> {
  return b64url(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier)));
}

function stubDiscord(user: { id: string; username: string; global_name?: string | null }) {
  return vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    if (url === "https://discord.com/api/oauth2/token") {
      return Response.json({ access_token: "discord-access" });
    }
    if (url === "https://discord.com/api/users/@me") return Response.json(user);
    return new Response("unexpected", { status: 500 });
  });
}

function raw(path: string, init: RequestInit = {}): Promise<Response> {
  return exports.default.fetch(`https://packs.test${path}`, { redirect: "manual", ...init });
}

async function start(verifier: string, redirect = APP, state = "app-state") {
  const q = new URLSearchParams({
    redirect_uri: redirect,
    state,
    code_challenge: await challengeFor(verifier),
  });
  return raw(`/auth/start?${q}`);
}

async function runToAppCode(verifier: string): Promise<string> {
  const res = await start(verifier);
  const loginId = new URL(res.headers.get("location")!).searchParams.get("state")!;
  const cb = await raw(`/auth/callback?code=discord-code&state=${loginId}`);
  expect(cb.status).toBe(302);
  const loc = new URL(cb.headers.get("location")!);
  expect(loc.searchParams.get("state")).toBe("app-state");
  return loc.searchParams.get("code")!;
}

function postToken(code: string, verifier: string): Promise<Response> {
  return api("/auth/token", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ code, code_verifier: verifier }),
  });
}

it("full sign-in round trip", async () => {
  const fetchSpy = stubDiscord({ id: "d1", username: "alice", global_name: "Alice" });
  const verifier = "v".repeat(50);

  const res = await start(verifier);
  expect(res.status).toBe(302);
  const authorize = new URL(res.headers.get("location")!);
  expect(authorize.origin + authorize.pathname).toBe("https://discord.com/oauth2/authorize");
  expect(authorize.searchParams.get("client_id")).toBe(env.DISCORD_CLIENT_ID);
  expect(authorize.searchParams.get("response_type")).toBe("code");
  expect(authorize.searchParams.get("scope")).toBe("identify");
  expect(authorize.searchParams.get("redirect_uri")).toBe(`${env.PUBLIC_URL}/auth/callback`);
  expect(authorize.searchParams.has("prompt")).toBe(false);

  const loginId = authorize.searchParams.get("state")!;
  const cb = await raw(`/auth/callback?code=discord-code&state=${loginId}`);
  expect(cb.status).toBe(302);
  const code = new URL(cb.headers.get("location")!).searchParams.get("code")!;
  const tokenCall = fetchSpy.mock.calls.find(([u]) => String(u).endsWith("/oauth2/token"))!;
  const body = new URLSearchParams(String((tokenCall[1] as RequestInit).body));
  expect(body.get("client_secret")).toBe(env.DISCORD_CLIENT_SECRET);
  expect(body.get("grant_type")).toBe("authorization_code");
  expect(body.get("code")).toBe("discord-code");
  expect(body.get("redirect_uri")).toBe(`${env.PUBLIC_URL}/auth/callback`);

  const tok = await postToken(code, verifier);
  expect(tok.status).toBe(200);
  const { token, user } = (await tok.json()) as { token: string; user: unknown };
  expect(user).toEqual({ id: "d1", name: "Alice", role: null });

  const me = await api("/me", {}, token);
  expect(me.status).toBe(200);
  expect(((await me.json()) as { id: string }).id).toBe("d1");

  const left = await env.DB.prepare("select id from logins where id = ?").bind(loginId).first();
  expect(left).toBeNull();
});

it("falls back to username when there is no global name", async () => {
  stubDiscord({ id: "d4", username: "bob", global_name: null });
  const verifier = "n".repeat(50);
  const tok = await postToken(await runToAppCode(verifier), verifier);
  expect(((await tok.json()) as { user: { name: string } }).user.name).toBe("bob");
});

it("owner id gets owner role", async () => {
  stubDiscord({ id: env.OWNER_DISCORD_ID, username: "boss" });
  const verifier = "o".repeat(50);
  const tok = await postToken(await runToAppCode(verifier), verifier);
  expect(((await tok.json()) as { user: { role: string } }).user.role).toBe("owner");
});

it("a former owner is demoted on sign-in", async () => {
  await env.DB.prepare("insert or replace into users (id, name, role, created_at) values ('d9', 'old', 'owner', 1)").run();
  stubDiscord({ id: "d9", username: "old" });
  const verifier = "q".repeat(50);
  const tok = await postToken(await runToAppCode(verifier), verifier);
  expect(((await tok.json()) as { user: { role: string } }).user.role).toBe("uploader");
});

it("non-loopback redirect is refused", async () => {
  const bad = [
    "http://evil.example/cb",
    "http://localhost:5000/cb",
    "https://127.0.0.1:5000/cb",
    "http://127.0.0.1/cb",
    "http://user:pw@127.0.0.1:5000/cb",
    "not a url",
  ];
  for (const redirect of bad) {
    const res = await start("x".repeat(50), redirect);
    expect(res.status, redirect).toBe(400);
    expect(((await res.json()) as { error: string }).error).toBe("bad_redirect");
  }
  const res = await raw("/auth/start?redirect_uri=" + encodeURIComponent(APP));
  expect(res.status).toBe(400);
  expect(((await res.json()) as { error: string }).error).toBe("bad_request");
});

it("wrong verifier is refused", async () => {
  stubDiscord({ id: "d2", username: "carol" });
  const code = await runToAppCode("right".repeat(10));
  const res = await postToken(code, "wrong".repeat(10));
  expect(res.status).toBe(400);
  expect(((await res.json()) as { error: string }).error).toBe("bad_login");
});

it("login codes work once", async () => {
  stubDiscord({ id: "d3", username: "dave" });
  const verifier = "once".repeat(12);
  const code = await runToAppCode(verifier);
  expect((await postToken(code, verifier)).status).toBe(200);
  const again = await postToken(code, verifier);
  expect(again.status).toBe(400);
  expect(((await again.json()) as { error: string }).error).toBe("bad_login");
});

it("expired login shows a try again page", async () => {
  stubDiscord({ id: "d5", username: "erin" });
  const res = await start("e".repeat(50));
  const loginId = new URL(res.headers.get("location")!).searchParams.get("state")!;
  await env.DB.prepare("update logins set expires_at = ? where id = ?")
    .bind(Math.floor(Date.now() / 1000) - 1, loginId)
    .run();
  const cb = await raw(`/auth/callback?code=discord-code&state=${loginId}`);
  expect(cb.status).toBe(400);
  expect(cb.headers.get("content-type")).toContain("text/html");
  expect(await cb.text()).toContain("try again");
});

it("discord error cancels the login", async () => {
  const res = await start("c".repeat(50));
  const loginId = new URL(res.headers.get("location")!).searchParams.get("state")!;
  const cb = await raw(`/auth/callback?error=access_denied&state=${loginId}`);
  expect(cb.status).toBe(400);
  expect(cb.headers.get("location")).toBeNull();
  const row = await env.DB.prepare("select id from logins where id = ?").bind(loginId).first();
  expect(row).toBeNull();
});

it("discord failure is a 502", async () => {
  vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response("no", { status: 401 }));
  const res = await start("f".repeat(50));
  const loginId = new URL(res.headers.get("location")!).searchParams.get("state")!;
  const cb = await raw(`/auth/callback?code=x&state=${loginId}`);
  expect(cb.status).toBe(502);
  expect(((await cb.json()) as { error: string }).error).toBe("discord_failed");
});

it("logout ends the session", async () => {
  stubDiscord({ id: "d6", username: "fay" });
  const verifier = "l".repeat(50);
  const tok = await postToken(await runToAppCode(verifier), verifier);
  const { token } = (await tok.json()) as { token: string };

  expect((await api("/auth/logout", { method: "POST" })).status).toBe(401);
  expect((await api("/auth/logout", { method: "POST" }, token)).status).toBe(200);
  expect((await api("/me", {}, token)).status).toBe(401);
});
