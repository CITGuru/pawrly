# SQL Over APIs: The Missing Layer Between MCP and ETL

<!-- published: 2026-06-23 -->
<!-- draft: true -->

![A glowing data center corridor representing connected systems](https://images.unsplash.com/photo-1558494949-ef010cbdcc31?q=80&w=2400&auto=format&fit=crop)

MCP made it easier to give agents tools. It did not make business data easier to query.

That difference is easy to miss in the current agent gold rush. A team wires up a few MCP servers, gives an assistant access to GitHub, Linear, Stripe, Slack, and an internal API, then asks a normal operational question:

> Which high-value customers are blocked by unresolved engineering work?

The agent can now reach the systems. That is progress. But access is not the same thing as understanding.

It still has to know which tool to call first, how to page through results, which fields identify the same customer across systems, whether a filter runs server-side or in memory, and how to combine the records without inventing a join. If the answer is wrong, it is hard to tell whether the model reasoned badly or the plumbing quietly shaped the data badly.

This is the gap between raw tools and reliable answers.

## The stack is splitting in two

The modern data stack is being pulled in two directions.

On one side are agents. They need fresh context from the systems where work actually happens: tickets, commits, invoices, incidents, support threads, usage events, and internal services. For that world, MCP is a useful standard. It gives agents a common way to discover tools and call them.

On the other side are pipelines. Data teams still need durable reporting, governed schemas, historical snapshots, metric consistency, and warehouses that can serve dashboards without hammering live APIs. For that world, ETL and ELT are still the right foundation.

Both are necessary. But there is a large middle space neither one handles well.

Not every question should become a pipeline. Not every answer should be improvised from raw tool calls.

Sometimes you just need to query the systems you already have.

## The missing layer is a query surface

Most business questions become relational as soon as you say them out loud.

Which customers have open issues? Which repositories have stale pull requests? Which incidents affected accounts with active contracts? Which invoices correspond to support escalations? Which API response should be joined with this CSV someone exported yesterday?

Those questions are not naturally expressed as a chain of tool calls. They are naturally expressed as joins, filters, projections, and aggregates.

That is why SQL keeps showing up. SQL is not popular because it is fashionable. SQL survives because it gives people and machines a shared way to describe the shape of an answer.

The missing layer between MCP and ETL is SQL over APIs: a query surface where live systems can be exposed as tables and functions before anyone decides whether the data deserves a permanent pipeline.

```sql
SELECT
  c.name,
  COUNT(i.id) AS open_issues
FROM customers.accounts c
JOIN linear.issues i
  ON i.customer_id = c.id
WHERE i.status != 'done'
GROUP BY c.name
ORDER BY open_issues DESC;
```

The important part is not the syntax. It is the contract. A table has columns. A function has arguments. A query can be logged, inspected, rerun, materialized, and improved.

That is a very different operating model from asking an agent to stitch arbitrary tool responses together in memory.

## MCP is the door, not the room

MCP is strongest when it gives an agent access to capabilities: search this system, fetch that record, create a ticket, send a message, inspect a repository.

But analytical context wants structure. A list of tools does not tell an agent how two systems relate. It does not define which fields are safe to join. It does not say whether "customer" means account, organization, buyer, workspace, or tenant. It does not give you a reusable query that a human can review later.

That does not make MCP wrong. It means MCP should often be the door into a better data interface.

In Pawrly, MCP clients can talk to the same query engine humans use. An agent can list sources, inspect tables, describe columns, call declared functions, run SQL, and materialize useful results. The tool call still exists, but the work happens against a queryable workspace instead of a pile of disconnected API shapes.

The agent stops spending most of its effort on integration mechanics and starts operating on a surface designed for questions.

## ETL is the warehouse, not the workbench

ETL solves a different problem. It turns operational data into durable analytical data.

That is exactly what you want for historical reporting, high-volume joins, compliance analysis, recurring dashboards, and shared metrics that have to stay consistent across the company. If an executive dashboard depends on a number every morning, that number probably belongs in a warehouse, a lake, or a materialized model.

But the first version of a question rarely deserves that much machinery.

A product manager wants to know whether customer escalations correlate with stale pull requests. A support lead wants to compare a CSV export with live Stripe customers. An engineer wants to check whether a GitHub search result matches tickets in Linear. An agent wants enough context to decide what to investigate next.

Those are workbench questions. You need a place to explore them before you turn them into infrastructure.

SQL over APIs gives teams that place.

## Where Pawrly fits

Pawrly treats APIs, files, databases, warehouses, lakehouse tables, and MCP servers as sources in one workspace. A REST endpoint can become a table. A search endpoint can become a table-valued function. An OpenAPI spec can seed a set of queryable tables. A useful result can stay live, be cached, or be pinned as a materialized table.

That gives you a path from exploration to reuse without changing interfaces halfway through.

Start with a live endpoint:

```sql
SELECT *
FROM github.pulls
WHERE owner = 'CITGuru'
  AND repo = 'pawrly'
  AND state = 'open';
```

Turn a call-shaped API into a function:

```sql
SELECT number, title, user_login
FROM github.search_issues('is:open label:bug repo:CITGuru/pawrly', 50);
```

Pin a result when it becomes useful enough to keep:

```sql
SELECT *
FROM materialized.top_customers
ORDER BY total DESC;
```

The same workspace can be used from the CLI, the console, an MCP client, or a library integration. Humans and agents are no longer working through separate models of the same systems.

## A better first step than glue code

Without a query layer, the default answer to every new API question is glue code.

Call Stripe. Call Linear. Parse JSON. Normalize IDs. Load a file. Join the data. Export the result. Add a README. Add retries. Add pagination. Then do it again when the question changes.

That is fine once or twice. It becomes expensive when every team and every agent repeats the same pattern.

With a SQL layer, the reusable part moves out of the script. The connection, auth, response shape, safety rules, and function arguments live in configuration. The question becomes a query.

That query might stay exploratory. It might become a saved example. It might become a materialized table. It might reveal that the source deserves a proper ETL pipeline. The point is that you do not have to make that decision before asking the first question.

## What this does not replace

SQL over APIs is not a replacement for warehouses. It is not a replacement for application APIs. It is not a replacement for MCP tools that perform actions.

If you need years of history, use a warehouse or lake. If you need strict dashboard latency, cache or materialize the result. If an agent needs to create, update, or delete a record, use the direct tool built for that action.

Pawrly is for the read path: the context-gathering layer where humans and agents need to ask clear questions across messy systems.

That read path has been underserved. Teams either overbuild it as ETL too early or underbuild it as one-off scripts and tool calls.

There is room for a middle layer.

## The agentic data stack needs boring interfaces

The best interface for agentic data work may not be new.

Agents need access, but access alone produces tool sprawl. Teams need durable data, but durability alone produces pipeline ceremony. Between those two is a simpler need: a stable way to query the systems where work is happening right now.

That is what SQL over APIs gives you.

Not SQL instead of MCP.

Not SQL instead of ETL.

SQL as the shared query surface between them.

Try Pawrly:

```sh
curl -fsSL https://pawrly.dev/install.sh | sh
```

Pawrly is open source: [github.com/CITGuru/pawrly](https://github.com/CITGuru/pawrly).
