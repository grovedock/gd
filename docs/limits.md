# Plans and limits

Every plan has every feature. Plans differ in how many people and agents, how much storage and
how many calls a minute. Prices are on [the pricing page](https://grovedock.com/pricing.html).

## Plans

| | Free | Team | Enterprise |
|---|---|---|---|
| People | 3 | as many as you buy | by licence |
| Active agents a month | 5 | not capped; beyond the number you buy, billed per agent for that month | by licence |
| Storage | 1 GB | 50 GB | your own servers |
| Private repositories | 5 (public ones unlimited) | unlimited | unlimited |
| Calls a minute, per person or agent | 300 | 3,000 | your own servers |

An organisation's **Usage & plan** tab shows what it uses against each limit, and **Change
plan** opens the checkout. Paying turns Team on at once.

## What happens at a limit

Nothing is ever deleted, and reading always works.

- **People:** nobody new can join until you buy more seats or remove someone.
- **Active agents:** an agent counts once it lands a change in a calendar month (UTC). At the
  limit, new agents can't land until next month; agents already active keep landing.
- **Storage:** measured every hour. Over the limit, nothing new can be pushed.
- **Private repositories:** new ones must be public.
- **Calls a minute:** beyond the allowance, calls are refused with `RESOURCE_EXHAUSTED`, saying
  how many seconds to wait. Anonymous readers of a public repository share four times one
  person's allowance.
- **Unpaid Team plan:** after a short grace period the organisation returns to Free. If it
  uses more than Free allows, it becomes read-only until you pay or reduce it.

## Other limits

| What | Limit |
|---|---|
| Organisations one person may create | 3 |
| Access token lifetime | 90 days by default, a year at most (`gd token create --days`) |
| Browser session | 30 days; signing out revokes it |
| Email sign-in link | 15 minutes, once |
| Webhooks per repository | 20, keeping each one's last 200 deliveries |
| One call's running time | 600 s |
| One request message | 64 MiB; files of 8 MiB and more upload in chunks, so this never limits file size |
