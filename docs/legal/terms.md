# Grovedock Terms of Service

**Effective date:** 19 October 2026
**Last updated:** 19 October 2026

These terms are an agreement between you and **Kabir Narang**, a sole proprietorship based in India, trading as "Grovedock" ("Grovedock", "we", "us"). Our address is [REGISTERED ADDRESS] and you can reach us at grievance@grovedock.com.

They cover **Grovedock**, our hosted service at https://cloud.grovedock.com (the "Service"). They do not cover gd, the open-source protocol, command-line tool and SDKs, which you may use under the Apache-2.0 licence included with them (see section 14), or Grovedock Enterprise on your own servers, which is covered by its own agreement.

If you use the Service on behalf of a company or other organisation, you confirm you are allowed to accept these terms for it, and "you" means that organisation.

## 1. Short version

- You own your code. We only use it to run the Service for you.
- We never train AI models on your code and we don't look at it except to run, support or secure the Service, or when the law makes us.
- Grovedock is new. We work hard to keep it up, but we don't promise a specific uptime yet.
- Pay for what you use, cancel when you want, export your data before you leave.
- If something goes wrong, our total liability is limited to what you paid us in the last 12 months.

The rest of this page is the full version, and it is what applies.

## 2. Who can use the Service

- You must be at least **18 years old** and able to enter a binding contract under Indian law. We do not knowingly allow children (under 18) to create accounts.
- Anyone may use the Free plan. Paid plans are billed in Indian rupees through Razorpay.
- The Service is designed for business and professional use (software development).

## 3. Your account

- You sign in with Google or with a one-time email link. Keep access to that Google account or mailbox secure, because anyone who controls it can get into your Grovedock account.
- Give us accurate information and keep your email address current; we send billing and security notices there.
- You are responsible for what happens under your account, including actions taken by AI agents, API tokens and team members you add.
- Tell us straight away at grievance@grovedock.com if you think your account or a token has been compromised.
- Organisation owners/admins control who belongs to the organisation and what they can access.

## 4. Acceptable use

You must not use the Service to:

1. Host, distribute or command malware, ransomware, botnets, phishing kits or exploit code aimed at systems you are not authorised to test.
2. Store or share content that is illegal in India, including content listed in Rule 3(1)(b) of the Information Technology (Intermediary Guidelines and Digital Media Ethics Code) Rules, 2021 (for example: child sexual abuse material, content that infringes someone else's intellectual property, content that impersonates another person, or content that threatens the unity, security or sovereignty of India).
3. Upload code, data or secrets you have no right to use, or personal data you have no lawful basis to process.
4. Attack, probe, overload or bypass the security of the Service or other customers (for example denial-of-service attacks, scraping other customers' data, testing vulnerabilities without our written permission).
5. Exceed or get around **rate limits**, quotas or plan limits, including by creating extra accounts or agents to avoid them.
6. **Resell, sublicense, rent or white-label** the Service, or offer it to third parties as a hosted service, without a separate written agreement with us.
7. Use the Service to send spam or unsolicited messages.
8. Mine cryptocurrency or use the Service as general-purpose file storage or a content delivery network unrelated to source control.
9. Harass or abuse our staff or other users.

You are responsible for the AI agents you connect. If an agent you run breaks these rules, that counts as you breaking them.

We may remove content or restrict access that breaks this section or the law (see section 12).

## 5. Your content

**"Customer Content"** means the source code, repositories, files, commit history, issues, metadata and other material you or your agents put into the Service.

- **You own your Customer Content.** We claim no ownership of it.
- You give us a limited, non-exclusive, worldwide, royalty-free licence to host, copy, store, process, back up, transmit and display your Customer Content **only as needed to provide, secure and support the Service for you** (and for anyone you share it with). This licence ends when the Customer Content is deleted from our systems (section 11).
- You confirm you have the rights needed to put your Customer Content into the Service and to give us this licence.
- If your Customer Content contains personal data about other people, you are responsible for having a lawful basis for that. For business customers, our [Data Processing Addendum](./dpa.md) applies.

## 6. Confidentiality of your code

We treat your Customer Content as confidential. Specifically:

- **We do not use your Customer Content to train, fine-tune or evaluate any artificial intelligence or machine-learning model**, ours or anyone else's, and we do not let our sub-processors do so.
- **We do not access your Customer Content except**:
  1. to operate the Service (automated processing that the Service performs for you, such as storing, merging, indexing and backing up);
  2. to give you support **when you ask us to**, and only as much as needed for that request;
  3. to investigate and protect against security incidents, abuse or breaches of these terms; or
  4. when required by law, court order or a lawful direction from a government authority. Where the law allows, we will tell you before we disclose your Customer Content.
- We do not sell your Customer Content or share it with advertisers.
- Only the owner has administrative access to production systems, and every access is logged.

We may use aggregated, anonymised usage statistics that do not contain your code or identify you (for example "number of lands per day across the Service") to run and improve the Service.

## 7. Service levels and beta

- Grovedock is offered on a **best-effort basis**. We aim for high availability but **we do not currently offer a service level agreement (SLA)**, uptime guarantee or service credits.
- We may carry out planned maintenance. We will try to give notice of planned downtime by email and on https://grovedock.com.
- Some features may be labelled **"beta", "preview" or "experimental"**. They may change, break or be removed, and are provided "as is". Do not rely on beta features for critical work.
- **Keep your own copies of important code.** Grovedock is source control: you can and should keep clones or exports of your repositories outside the Service.

## 8. Plans and pricing

Current prices are shown at https://grovedock.com/pricing.html. At launch:

| Plan | Price | Notes |
|---|---|---|
| **Free** | ₹0 | Limited capacity; access may be granted through a waitlist. Limits are shown in the product. |
| **Team** | ₹149 per active agent per month **plus** ₹599 per person per month | Billed monthly, or annually in advance. |

Prices are in Indian rupees. While we are not registered for GST, no GST is charged (see section 9).

**Active agent.** An "active agent" is an AI agent identity (or API token used by an automated agent) in your organisation that lands at least one change in your organisation during a calendar month (UTC), directly or through a change request it wrote. Agents that land nothing in a month are not charged.

**Person.** A "person" is a human member of your organisation for whom you have bought a seat. Adding people beyond the seats you bought needs more seats first.

**Fair use.** Each paid active agent includes a fair-use allowance of **3,000 lands per month** (a "land" is a change successfully merged into a repository on the Service). This is **not a charge or a hard cap**. If you regularly go well above it, we will contact you to talk about your usage and a suitable plan. We will not bill you for extra lands without your agreement. We may apply technical rate limits to protect the Service for everyone.

**Free tier.** The Free plan has capped capacity. We may limit the number of free accounts, put new sign-ups on a waitlist, and change free-tier limits with reasonable notice.

**Price changes.** We may change prices with at least **30 days' notice** by email. Changes apply from your next billing period (for annual plans, from your next renewal). If you don't agree, you can cancel before the change takes effect.

## 9. Billing, payment and taxes

- Payments are processed by **Razorpay**. We do not see or store your full card number, UPI PIN or bank login; Razorpay handles that under its own terms.
- **Monthly plans** are charged in advance at the start of each month. **Annual plans** are charged in advance for 12 months. Plans **renew automatically** unless cancelled before the renewal date. For monthly plans, RBI rules on recurring payments may require a notice before each debit and extra authentication; Razorpay and your bank handle these.
- Usage-based amounts (active agents added during a period) are billed in arrears, after the month ends.
- **Taxes.** We are not currently registered for GST, so no GST is charged and we cannot issue a tax invoice that supports input tax credit. If we register, we will tell you in advance and GST will be added from that date.
- Razorpay emails a receipt for every payment.
- Refunds are covered by our [Refund and Cancellation Policy](./refund-and-cancellation.md).

## 10. Non-payment and suspension

- If a payment fails, we will email you and retry. If it is still unpaid **14 days** after the due date, we may downgrade you to the Free plan or **suspend** paid features.
- If still unpaid **30 days** after the due date, we may suspend the account. While suspended you can still **export** your data for at least 30 more days (the organisation becomes read-only).
- We may also suspend access immediately, with notice where practical, if: you seriously breach section 4; your use threatens the security or stability of the Service; or we are required to by law. We will restore access once the problem is fixed, where appropriate.

## 11. Ending the agreement; export and deletion

**You** can stop using the Service and delete your organisation/account at any time from Tokens, in the account menu or by emailing grievance@grovedock.com. Cancelling a paid plan stops future renewals; see the Refund and Cancellation Policy for money.

**We** can end these terms (a) with **30 days' notice** for any reason, in which case we refund any prepaid fees for the unused period, or (b) immediately if you materially breach these terms and do not fix the breach within 15 days of notice (or immediately for serious acceptable-use breaches or illegal content).

**What happens to your data:**

| When | What happens |
|---|---|
| Before you delete / during notice | You can export your repositories at any time (for example with `gd clone`, or `gd export-git` for a git repository with the same commits). |
| You delete a repository or account | It is removed from the live Service immediately and permanently deleted from primary storage within **30 days**. |
| Backups | Encrypted backups containing it roll off and are deleted within **35 days** after deletion from primary storage. |
| We end the agreement | We keep your data available for export for **30 days** after termination (unless the law or a court order stops us), then delete it on the schedule above. |
| Legal holds | We may keep data longer only where the law requires it (for example records needed for tax or a legal claim), and only that data. |

Sections 5 (licence ends on deletion), 6, 9 (unpaid amounts), 15, 16, 17 and 18 survive termination.

## 12. Complaints about content and takedowns

Because the Service stores content uploaded by customers, we may be treated as an "intermediary" under the Information Technology Act, 2000. If you believe content on the Service is unlawful or infringes your rights, contact our **Grievance Officer** (see [Contact](./contact.md)). We will acknowledge complaints within **24 hours** and aim to resolve them within **7 days** (or faster where the law requires — for example, for orders from a court or authorised government agency, and for certain categories of harmful content, where the law sets timelines measured in hours). When we remove or disable access to your content, we will tell you why where we are allowed to, and you can respond or dispute it through the same Grievance Officer.

## 13. Changes to the Service

We may add, change or remove features. If we remove a feature that is a material part of a paid plan, we will give you at least 30 days' notice, and if that harms you materially you can cancel and receive a pro-rata refund for the unused prepaid period.

## 14. Open-source software

gd (the protocol, the `gd` command-line tool, the git bridge and the SDKs) is open source at https://github.com/grovedock/gd under the Apache-2.0 licence. Nothing in these terms limits your rights under that licence. The Service itself, including its server and web app, is ours and is not open source.

## 15. Warranty disclaimer

The Service is provided **"as is" and "as available"**. To the extent the law allows, we disclaim all warranties not expressly stated in these terms, including implied warranties of merchantability, fitness for a particular purpose and non-infringement. We do not promise that the Service will be uninterrupted, error-free or that data will never be lost. Nothing in these terms excludes rights you have under law that cannot be excluded (for example, rights under the Consumer Protection Act, 2019, if you are a consumer).

## 16. Limitation of liability

To the extent the law allows:

- Neither party is liable for indirect, incidental, special or consequential losses, or for loss of profits, revenue, goodwill or business opportunity, even if told they were possible.
- Each party's **total liability** arising out of or relating to these terms is limited to the **fees you paid us for the Service in the 12 months before the event giving rise to the claim** (for the Free plan: ₹1,000).
- These limits do not apply to: your payment obligations; your breach of section 4; either party's fraud or wilful misconduct; or anything that cannot be limited by law.

## 17. Indemnity

You will defend and compensate us against third-party claims (and resulting reasonable costs, damages and fines) to the extent they arise from: (a) Customer Content you upload that you had no right to upload or that is unlawful; or (b) your breach of section 4 (Acceptable use). We will promptly tell you about the claim, let you control the defence, and cooperate reasonably. You may not settle a claim in a way that admits fault on our behalf without our consent.

## 18. Governing law and disputes

These terms are governed by the laws of **India**. Before going to court, both parties will try in good faith to resolve a dispute by email for 30 days. Subject to that, the courts at **Karnal, Haryana, India** have exclusive jurisdiction.] This does not stop a consumer from using a consumer forum where the law allows.

## 19. Changes to these terms

We may update these terms. For material changes we will email account owners and post a notice in the product at least **30 days** before the changes take effect (unless a change is required sooner by law). If you keep using the Service after the effective date, the new terms apply. If you don't agree, you can cancel before they take effect and get a pro-rata refund of any prepaid fees for the unused period.

## 20. General

- **Notices** to you go to your account email; notices to us go to grievance@grovedock.com and [REGISTERED ADDRESS].
- **Assignment:** you may not transfer these terms without our consent; we may transfer them to a successor of the business (for example if the sole proprietorship is converted into a company), with notice to you.
- **Force majeure:** neither party is liable for delays caused by events beyond reasonable control (except payment obligations).
- **Entire agreement:** these terms, the Privacy Policy, the Refund and Cancellation Policy, the Delivery Policy and (for business customers) the DPA are the whole agreement for the Service. If they conflict, the DPA wins on personal-data matters.
- **Severability / no waiver:** if a clause is unenforceable the rest still applies; not enforcing a clause is not a waiver.

**Contact:** Kabir Narang, [REGISTERED ADDRESS], grievance@grovedock.com. See also [Contact](./contact.md).
