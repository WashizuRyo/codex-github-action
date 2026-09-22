const routes = new Set(["GET /healthz", "POST /github/webhook"]);

function jsonResponse(body, status) {
  return Response.json(body, {
    status,
    headers: { "cache-control": "no-store" }
  });
}

export async function handleRequest(request, env) {
  const url = new URL(request.url);
  const route = `${request.method} ${url.pathname}`;

  if (!routes.has(route)) {
    return jsonResponse({ error: "not found" }, 404);
  }

  const deliveryId = request.headers.get("x-github-delivery");
  console.log(
    JSON.stringify({
      message: "forwarding request to local bridge",
      method: request.method,
      path: url.pathname,
      deliveryId
    })
  );

  try {
    const target = new URL(`${url.pathname}${url.search}`, "http://localhost");
    const upstreamRequest = new Request(target, request);
    return await env.BRIDGE.fetch(upstreamRequest);
  } catch (error) {
    console.error(
      JSON.stringify({
        message: "local bridge unavailable",
        path: url.pathname,
        deliveryId,
        error: error instanceof Error ? error.message : String(error)
      })
    );
    return jsonResponse({ error: "bridge unavailable" }, 502);
  }
}

export default {
  fetch(request, env) {
    return handleRequest(request, env);
  }
};
