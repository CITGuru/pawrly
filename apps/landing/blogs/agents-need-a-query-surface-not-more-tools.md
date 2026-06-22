# Agents Need a Query Surface, Not More Tools

![A dog lying on its back against a white background](https://cdn-images-1.medium.com/v2/resize:fit:2400/0*ft6ONVaQ3dY9GsX-)

I keep coming back to the same problem with agents.

They are already better at deciding what to do most times, but the moment they need real context they fall into plumbing.

Ask an agent a normal business question:

> Which paying customers have not heard from support recently?

The question is not complicated. A person knows roughly where the answer lives. Payments are probably in Stripe. Support or conversations might be in Intercom, Zendesk, Linear, or some internal tool. There might also be a CSV export someone uses because the real system is missing one field.

But for an agent, that simple question turns into a pile of tiny integration problems.

How do I call Stripe? Where is the token? How does pagination work? What does the JSON look like? How do I call the support system? Do both systems use the same email field? Is the file local, in S3, or attached somewhere? Did I accidentally join the wrong thing?

At that point the agent is not really doing analysis. It is writing glue code and eating through your tokens. Sometimes it gets the answer right. Often it gets only part of the way there.

That feels backwards to me.

## The Interface Matters More Than the Tool Count

A lot of agent setups solve this by giving the agent more tools. Add a Stripe tool. Add a GitHub tool. Add a Linear tool. Add a file reader. Add a database tool. Add an internal API tool.

That works up to a point, but it also creates a new problem: every tool has its own shape.

The agent has to learn each one in context. It has to decide when to call which tool. It has to stitch results together in memory. It has to remember the rules around access and filtering. And if the result is wrong, it is hard to know whether the reasoning failed or the plumbing failed.

I have come to realize that agents actually need fewer shapes, not more.

That is the idea behind Pawrly: describe the sources once, then let agents query them through one SQL interface.

APIs become tables. Files become tables. MCP servers can become tables. The agent asks a question against a stable interface instead of improvising an integration every time.

```sql
SELECT c.email,
       c.name,
       i.last_seen_at
FROM stripe.customers c
JOIN intercom.contacts i ON i.email = c.email
ORDER BY i.last_seen_at ASC;
```

That is the kind of thing I want an agent to do. Not because SQL is magic, but because the shape is predictable. You can inspect it. You can log it. You can rerun it. You can put rules around it.

## APIs Are Where a Lot of Work Actually Lives

Most agent demos eventually need data from APIs.

The important context is not always in a warehouse. It is in Stripe, GitHub, Linear, HubSpot, Notion, an internal service, or a random endpoint only one team remembers. And a lot of those APIs already have a contract: an OpenAPI spec, a response shape, a documented list endpoint.

Pawrly takes that seriously. A REST or GraphQL endpoint can be exposed as a table. If there is an OpenAPI spec, Pawrly can use it to create tables from the documented GET operations. If there is an MCP server, Pawrly can consume it as a source too.

That last part is important. Pawrly is not trying to replace MCP. It fits into the MCP world, so new or existing MCP servers are supported out of the box.

It can read from MCP servers as sources, and it can also run as an MCP server itself. So Cursor, Claude Desktop, Codex, or another client can ask Pawrly questions over the same workspace you use locally.

In practice, that means you can connect the systems once and give agents one controlled place to ask for data.

## Files Should Not Be Special Either

The other place agents need context is files.

CSV exports. JSON dumps. Parquet datasets. Files in S3. Files on disk. The kind of things teams actually pass around when the "real" integration does not exist yet.

Those should not require a separate mental model. If a file has rows, the agent should be able to query it alongside the API data.

That is why Pawrly treats files as part of the same interface. A local CSV can join against a Parquet dataset in object storage and a live API response. The agent does not need to care where each row came from. The configuration handles that.

## The Point Is Not Just Connection

It is easy to make data reachable. It is harder to make it safe and repeatable.

That is where the governance part matters.

If an agent is going to answer questions about revenue, customers, usage, or support activity, I do not want it inventing definitions on the fly. I want the business vocabulary to be declared somewhere. I want access rules applied automatically. I want bad joins to fail instead of returning a number that looks believable.

Pawrly has a semantic layer for that. You can define measures, dimensions, relationships, required filters, and safety rules. Then agents query through that layer instead of guessing how the business works from raw columns.

This does not make agents perfect. It does something more useful: it removes a bunch of boring, dangerous ways for them to be wrong.

## Where Pawrly Fits

There are still plenty of cases where a direct tool call is the right thing. If an agent needs to create a ticket, send a message, or update a record, it should probably call the tool built for that action.

Pawrly is for the read path: the part where an agent needs context before it decides what to do.

That read path benefits from being consistent. The same source definitions can be reused by the CLI, the console, and MCP clients. Queries can be inspected after the fact. Access rules can live in config instead of being repeated in prompts. Files and APIs can be joined without turning every workflow into a small integration project.

That is the practical goal: make it easier to give agents useful context without giving up control over how that context is reached.

You can try out Pawrly below:

```sh
curl -fsSL https://pawrly.dev/install.sh | sh
```

Pawrly is open source: [github.com/CITGuru/pawrly](https://github.com/CITGuru/pawrly).