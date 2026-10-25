import type { Env } from "./env";
import { HttpError } from "./http";

const failed = () => new HttpError(502, "discord_failed", "Discord sign-in failed");

export async function exchangeCode(env: Env, code: string, redirectUri: string): Promise<string> {
  const res = await fetch("https://discord.com/api/oauth2/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      client_id: env.DISCORD_CLIENT_ID,
      client_secret: env.DISCORD_CLIENT_SECRET,
      grant_type: "authorization_code",
      code,
      redirect_uri: redirectUri,
    }),
  });
  if (!res.ok) throw failed();
  const body = (await res.json()) as { access_token?: string };
  if (!body.access_token) throw failed();
  return body.access_token;
}

export async function fetchUser(accessToken: string): Promise<{ id: string; name: string }> {
  const res = await fetch("https://discord.com/api/users/@me", {
    headers: { authorization: `Bearer ${accessToken}` },
  });
  if (!res.ok) throw failed();
  const body = (await res.json()) as { id?: string; username?: string; global_name?: string | null };
  if (!body.id) throw failed();
  return { id: body.id, name: body.global_name || body.username || body.id };
}
