# romulus Terms of Service

**Version:** 1.1
**Effective date:** 24 October 2026

These Terms of Service (the "**Terms**") govern access to and use of the hosted romulus service available at **https://romulus.lupidae.com** (the "**Service**"). They are also called general terms of use and sale (*conditions générales d'utilisation et de vente*).

**The Service is offered only to professionals, for their professional activity, on every Plan, including the Free plan (Section 3.5).**

These Terms are available in French at https://romulus.lupidae.com/fr/legal/conditions and in English at https://romulus.lupidae.com/legal/terms. The two versions have the same numbering. **In case of discrepancy, the French version prevails.**

## 1. Who we are

The Service is provided by:

- **Lucas Lefort, entrepreneur individuel (EI)**, trading as "romulus";
- address: 108 rue de Gand, 59200 Tourcoing, France;
- SIREN 949 815 138, SIRET 949 815 138 00019, registered in the French national business register (*Registre national des entreprises*, RNE);
- VAT: not applicable, VAT exemption under Article 293 B of the French General Tax Code (*TVA non applicable, art. 293 B du CGI*);
- email: **romulus@lupidae.com**; telephone: +33 7 82 98 35 07;
- director of publication: Lucas Lefort.

The legal notice is available at https://romulus.lupidae.com/legal-notice.

In these Terms, "**romulus**", "**we**", "**us**" and "**our**" mean the provider identified above and, after an assignment under Section 22, its successor.

## 2. Definitions

- "**Account**": a personal user account created to access the Service.
- "**Organization**": a workspace in the Service that groups Accounts, databases, schemas and the subscription to a Plan.
- "**Customer**" or "**you**": the person who accepts these Terms. Where an Account or an Organization is used on behalf of an employer, a company or another legal person, "you" also means that person.
- "**Business Customer**": a Customer, natural or legal person, public or private, acting for purposes relating to its trade, business, craft or profession, including through a person acting in its name or on its behalf, such as an employee or an agent (Section 3.5).
- "**Consumer**": a natural person acting for purposes outside their trade, business, craft or profession.
- "**Non-professional**": a legal person that is not acting for professional purposes.
- "**Customer Content**": all content submitted to the Service by you or for you, including schema definitions (DDL), drafts, versions, migrations, comments, approvals, and the queries and migrations run in Playgrounds.
- "**Playground**": an isolated, temporary PostgreSQL instance operated by romulus, filled with generated fake data, in which you can run queries and migrations.
- "**Plan**": the Free plan or one of the paid plans described in Section 7.1 and on the pricing page at https://romulus.lupidae.com/#pricing.
- "**Billing Period**": the month or the year for which a paid Plan is paid in advance.
- "**Software**": the romulus source code, published under the GNU Affero General Public License v3.0 ("**AGPL-3.0**").

## 3. Acceptance, scope and conclusion of the contract

3.1. **Acceptance.** You accept these Terms when you create your Account or first sign in to the Service, by ticking the acceptance box, and you confirm by ticking a second box that you use the Service for professional purposes (Section 3.5). You accept them again, and confirm your professional use again, when you subscribe to a paid Plan. If you accept on behalf of your employer, a company or another legal person, you confirm that you are authorized to bind it.

3.2. **Scope.** These Terms cover **only the hosted Service**. They do not govern your use of the Software when you install it yourself; that use is governed solely by the AGPL-3.0 (see Section 11).

3.3. **Age.** You must be at least 18 years old to create an Account.

3.4. **Steps to conclude the contract** (Articles 1127-1 and 1127-2 of the French Civil Code). The contract is concluded as follows:

a) **Account.** You create an Account on our sign-in service with your email address and a password, and you confirm your email address.

b) **Free plan.** When you create your Account or first sign in, you read these Terms, accept them by ticking a box, and confirm your professional use by ticking a second box. The contract for the Free plan is then concluded, for an indefinite period.

c) **Paid Plan.** An administrator of the Organization chooses a Plan and a Billing Period in the billing settings. A summary shows the Plan, its price, the Billing Period and the renewal terms. You accept these Terms and confirm your professional use by ticking the boxes described in Sections 3.1 and 3.5. You then click "Continue to payment".

d) **Payment page.** You are taken to a secure payment page operated by Stripe. It shows the order summary and the total price, and lets you enter or correct your billing details, including the name of your business and, where it has one, its VAT number, and your payment method. Until you confirm, you can go back and change the Plan, the Billing Period or your answers. **Clicking the payment button on this page ("Subscribe" or "Pay and subscribe") places an order with an obligation to pay.** The contract for the paid Plan is concluded when the payment is accepted.

e) **Confirmation.** We acknowledge the order by email without undue delay: Stripe sends the receipt or the invoice for us. On request, we send you a copy of the version of these Terms that you accepted (Section 3.6).

3.5. **Professional use only.**

a) The Service, on every Plan including the Free plan, is offered only to Business Customers, that is, to persons who use it for their trade, business, craft or profession. They include companies, sole traders and freelancers (including micro-entrepreneurs), members of the liberal professions, public bodies, and associations and other legal persons that use the Service for their professional activity.

b) **Employees and agents.** If you use the Service for your employer, or for another organization that you represent, you act in its name: that employer or organization is then the Business Customer, and you confirm that you are authorized to accept these Terms for it.

c) **Attestation.** When you create your Account, and when you subscribe to a paid Plan, you confirm by ticking a box that you use the Service for professional purposes and, if you act for an employer or another organization, that you are authorized to do so. When you subscribe to a paid Plan, the business named on the payment page is the Customer for that Plan.

d) **Consumers.** The Service is offered neither to Consumers nor to Non-professionals. Section 9 sets out what happens if one of them uses it nonetheless.

e) The Data Processing Agreement applies (Section 16).

3.6. **Language and archiving.** The contract can be concluded in French or in English. We keep a record of your acceptance and of your attestations (date, version of the Terms, language and, for a paid Plan, the Plan and the Billing Period) for the duration of the contract and five years after it ends, and we keep the invoices for 10 years (Section 17.6). You can obtain a copy at any time by writing to romulus@lupidae.com. Every version of these Terms remains available on request.

## 4. The Service

4.1. **What the Service does.** romulus is a hosted service for teams working with PostgreSQL. Depending on your Plan, it lets you:

- version database schemas;
- collaborate on drafts, with real-time collaborative editing, comments and approvals;
- generate migrations;
- push schemas from continuous integration (CI) pipelines, using an API token;
- run Playgrounds and migration tests on generated fake data.

4.2. **romulus never connects to your own databases.** The Service stores the schema definitions you provide, your comments and your Account and Organization data. Playgrounds are separate databases operated by romulus and filled with generated fake data. They are not copies of your data.

4.3. **Generated output.** Migrations and other output generated by the Service are aids. You remain responsible for reviewing, testing and approving any migration before you apply it to your own databases. romulus does not apply anything to your databases.

4.4. **Functionality and interoperability.** The Service works in a recent web browser with JavaScript enabled. Schemas are read and written as PostgreSQL SQL. Exports are ZIP files containing SQL and JSON files (Section 17.1). CI pushes use an HTTP API documented in the Service.

4.5. **Changes to the Service.** We may change or improve the Service, in particular for technical, security or legal reasons, or to add or improve features. Such changes are made at no extra cost to you, and we inform you of any significant change. If a change materially reduces your access to or use of the Service under a paid Plan, we tell you by email at least 30 days in advance, and you may terminate the contract free of charge within 30 days of the change and receive a refund of the prepaid fees for the unused part of the Billing Period.

4.6. **Hosting and international access.** The Service and its data are hosted in France, on infrastructure operated for us by a hosting provider in France; that infrastructure is subject to French law. The website and the network entry point of the Service run on Cloudflare, Inc., a company subject to the law of the United States, which carries the traffic between users and our servers. The providers that process personal data are listed in the Privacy Policy. To prevent an authority of a country outside the European Union from accessing or obtaining data held in the European Union where this would conflict with European Union or French law (Article 28 of Regulation (EU) 2023/2854, the "**Data Act**"):

- the Service's databases and their backups are kept only in France;
- data is encrypted in transit between users and Cloudflare, and between Cloudflare and our servers;
- we disclose data to an authority of a country outside the European Union only where an international agreement, European Union law or French law requires it, after checking that the request is lawful;
- we inform you of any such request concerning your data, unless the law forbids it.

## 5. Accounts and Organizations

5.1. You must provide accurate information and keep it up to date. You must keep your password confidential, and you are responsible for activity under your Account unless you show that it resulted from a fault of ours or from an unauthorized use you could not prevent. Tell us promptly at romulus@lupidae.com if you suspect unauthorized access.

5.2. API tokens issued by the Service must be stored securely and revoked when no longer needed. A revoked token stops working at once.

5.3. **Organizations.** An Organization is managed by its administrators. They can invite and remove members, set roles (administrator, developer, reviewer), manage the subscription, and export the Organization's content. Customer Content created within an Organization is held for the Customer that holds the Organization, not for the individual member who created it. When a member leaves or is removed, their contributions remain in the Organization, under their name.

5.4. The Customer that holds an Organization is responsible for its members' compliance with these Terms.

## 6. Acceptable use

6.1. You agree not to, and not to let anyone else:

a) use the Service in breach of applicable law or of third-party rights;
b) upload malware, or content that is unlawful, infringing, or that you do not have the right to submit;
c) gain or attempt to gain unauthorized access to the Service, to other Accounts or Organizations, or to our systems, or probe, scan or test their vulnerability, except as allowed by Section 6.4;
d) interfere with the integrity or performance of the Service, including by denial-of-service attacks or by generating excessive or intentionally abusive load;
e) circumvent caps, rate limits, Plan limits or billing, including by creating several Accounts or Organizations to evade the limits of a Plan;
f) resell or provide the hosted Service to third parties as a service, other than to the members of your own Organization. This does not restrict any right you have in the Software under the AGPL-3.0.

6.2. **Playgrounds.** Playgrounds are provided only to test schemas, queries and migrations. In particular, you must not:

a) attempt to escape or bypass the isolation of a Playground (container, sandbox, kernel or network), or to access the host, other Playgrounds or other customers' data;
b) use a Playground for cryptocurrency mining, password cracking or other compute-intensive work unrelated to testing a database schema;
c) use a Playground to send email or network traffic to third parties, to host content, or as a proxy, relay, scanner or attack platform;
d) put real personal data, production data or confidential data into a Playground, for example in migrations or in literal values typed into queries. Playgrounds are designed for generated fake data;
e) run workloads designed to exhaust shared resources (processor, memory, disk) beyond normal testing.

6.3. **How Playgrounds work.** A Playground stops after 15 minutes without activity and is deleted 7 days after its last use, or earlier when its draft is published or deleted. It is never backed up. We keep the text of the queries and migrations run in it (not their results) until the Playground's record is deleted, at most 30 days after its last use. We monitor resource usage and technical metadata of Playgrounds to enforce this Section and to protect the Service, and we may stop, reset or delete a Playground that breaches it.

6.4. **Security research.** If you believe you have found a vulnerability, report it to romulus@lupidae.com, following our security policy at https://github.com/lupidae/romulus/blob/main/SECURITY.md. If you act in good faith and follow that policy, in particular without accessing other customers' data beyond what is needed to show the vulnerability, without degrading the Service and without disclosing the vulnerability before it is fixed or the agreed embargo has ended, we will not treat your research as a breach of these Terms and will not bring legal proceedings or file a complaint against you for it. This commitment does not bind public authorities.

6.5. **Reporting illegal content.** Anyone can report content stored on the Service that they consider illegal by writing to romulus@lupidae.com. To be processed, a report must contain: a sufficiently substantiated explanation of why the content is illegal; its precise location (Organization, database and version, or the address of the page); the name and email address of the person reporting, except for reports of child sexual abuse material; and a statement that the person reporting believes in good faith that the report is accurate and complete. We acknowledge receipt, examine reports diligently and objectively, with human review and no automated decision, and inform the person reporting of our decision and of the available remedies. Knowingly reporting content as illegal in order to have it removed, when the information is inaccurate, is a criminal offence in France (Article 6 of Law No. 2004-575 of 21 June 2004, "LCEN").

6.6. **Our decisions on content and accounts.** If we remove or disable access to content, or suspend or terminate an Account or an Organization, because content is illegal or breaches these Terms, we inform the Customer concerned by email, stating the facts, the grounds and the remedies available, unless the law or an authority forbids it (Article 17 of Regulation (EU) 2022/2065, the "Digital Services Act"). You can contest the decision by replying to that email; the decision is then reviewed by a person. You can also go to court. Public authorities and users can contact us, in French or in English, at romulus@lupidae.com, which is our single point of contact under Articles 11 and 12 of the Digital Services Act.

## 7. Plans, prices and payment

7.1. **Plans.** The Service is offered under the following Plans. The table describes them as at the date of these Terms; the pricing page shows the Plans in force. Each subscription keeps the features and caps of its Plan as shown at the time of subscription, subject to Sections 4.5 and 21.

| | Free | Starter | Team | Business |
|---|---|---|---|---|
| Price per month | EUR 0 | EUR 19 | EUR 49 | EUR 149 |
| Price per year (10 months) | n/a | EUR 190 | EUR 490 | EUR 1,490 |
| Members (pending invitations included) | 1 | unlimited | unlimited | unlimited |
| Databases | 1 | 3 | 10 | 50 |
| Versions per database | 50 | 500 | unlimited | unlimited |
| Tables and views per schema version | 50 | 200 | 1,000 | 5,000 |
| History shown | 90 days | 365 days | unlimited | unlimited |
| Playground hours per calendar month (UTC) | 2 | 20 | 80 | 300 |
| Playground size (generated rows, approximately) | 50,000 | 50,000 | 1,000,000 | 1,000,000 |
| Required approvals per draft (up to) | none | 1 | 10 | 10 |
| Approval override by administrators | no | no | no | yes |
| Support | community | email | email | priority email |

Technical limits apply on every Plan to protect the Service, such as a maximum of 5 Playgrounds at a time per Organization and rate limits on requests.

7.2. **Caps.** Caps are hard limits. When a cap is reached, the Service refuses to create the next resource (for example a new database or version) or to start new Playground time, until usage is back under the cap or you upgrade. **Reaching a cap never deletes anything.** Versions older than the history shown by your Plan are hidden, not deleted: the latest version of each database stays visible, hidden versions are included in exports, and they are shown again after an upgrade. When the Playground hours of the month are used up, running Playgrounds are stopped and new runs wait for the next calendar month or an upgrade.

7.3. **Prices and taxes.** Prices are in euros, excluding taxes. As long as romulus benefits from the VAT exemption (*franchise en base*), no VAT is charged (*TVA non applicable, art. 293 B du CGI*), and the price you pay is the price shown. If VAT becomes applicable, for example because the exemption threshold is exceeded or after an assignment under Section 22, it is added at the applicable rate to the amounts invoiced from the date it becomes applicable. We tell you by email beforehand or, if the change could not be foreseen, as soon as possible; you can then cancel under Section 8.1. Customers established outside France may have to account for VAT in their own country (reverse charge).

7.4. **Payment.** Paid Plans are paid in advance for each Billing Period, monthly or yearly, using the payment methods offered on Stripe's payment page. romulus never receives your full card details. You authorize us, through Stripe, to charge your payment method at the start of each Billing Period. Payment is due on the first day of the Billing Period.

7.5. **Automatic renewal.** A paid subscription renews automatically for a new Billing Period of the same length (one month or one year), at the price in force, unless it is cancelled before the end of the current Billing Period (Section 8.1).

a) **Yearly Plans.** We send a dedicated email reminding you of the renewal date and of your right not to renew, about 40 days before the renewal, and in any case no earlier than three months and no later than one month before the end of the Billing Period. The email shows, in a visible box, the last date on which you can cancel.
b) **If we fail to send the reminder**, you can terminate the contract free of charge at any time from the renewal date. The prepaid fees for the period after the termination are then refunded within 30 days, after deducting the amount corresponding to the performance of the contract up to the termination.
c) **Monthly Plans** can be cancelled at any time, with effect at the end of the current month.

7.6. **Price changes.** We may change the prices of the Plans. A new price applies to an existing subscription only from its next renewal. We tell you by email at least 30 days before that renewal. If you do not accept the new price, you can cancel before the renewal, at no cost, and the old price applies until the end of the current Billing Period.

7.7. **Upgrades and downgrades.** An upgrade to a higher Plan takes effect immediately; the difference for the rest of the current Billing Period is charged at once, prorated by Stripe. A downgrade, or a switch from a yearly to a monthly Billing Period, takes effect at the end of the current Billing Period. A downgrade never deletes anything (Section 7.2).

7.8. **Failed payments.** If a payment fails, Stripe may try again, and we inform the Organization's administrators by email. The paid Plan is kept for **14 days from the due date**. We send a reminder 3 days before the end of that period. If the payment is still outstanding after 14 days, the Organization moves to the Free plan: nothing is deleted, and paying the outstanding invoice restores the paid Plan at once. We do not claim any payment for the time the Organization spends on the Free plan.

7.9. **Late payment.** Any amount not paid by its due date automatically bears, from the day after the due date and without a reminder being needed, late-payment interest at three times the French legal interest rate. A fixed indemnity of EUR 40 for recovery costs is also due for each unpaid invoice. If the recovery costs actually incurred are higher, additional compensation may be claimed on proof (Articles L441-10 and D441-5 of the French Commercial Code).

7.10. **Invoices.** Invoices are issued electronically and are available by email and in the billing portal ("Manage billing"). They state that VAT is not applicable under Article 293 B of the French General Tax Code. When electronic invoicing becomes mandatory for us, invoices to businesses established in France will be issued in the required electronic form, and you agree to give us the information it requires, such as your SIREN number.

7.11. **Founding-team offer.** The first 20 Organizations to subscribe to the Team Plan through the founding-team offer (the link on the pricing page, while it shows places left) pay EUR 29 per month or EUR 290 per year instead of the price of the Team Plan. This price is fixed: Section 7.6 does not apply to it, for as long as the subscription stays active on the Team Plan without interruption. Switching between the monthly and the yearly Billing Period keeps it. The offer ends for good when the subscription is cancelled (at the end of its Billing Period, Section 8.1), when the contract ends (Sections 9.1 and 15), or when the subscription moves to another Plan; a later subscription to the Team Plan is at its price in force. The offer can be used once per Organization. A place is held for you while the payment page is open, for 30 minutes at most.

## 8. Cancellation and refunds

8.1. **Cancellation.** You can cancel a paid subscription at any time, free of charge, from the billing settings of your Organization, with the "Cancel subscription" button, then confirm on Stripe's page. You can also cancel by email to romulus@lupidae.com, from the address of an administrator of the Organization. Cancellation takes effect at the end of the current Billing Period. We confirm it by email, stating the date on which it takes effect and its consequences. Until that date you keep the paid features. The Organization then moves to the Free plan, and nothing is deleted.

8.2. **Refunds.** Fees paid are not refundable, and no refund is given for a partial Billing Period or unused features, except:

a) where the contract ends under Section 9.1 (persons who are not professionals);
b) under Sections 4.5 (changes to the Service), 7.5 b) (missing renewal reminder), 14.2 (suspension not caused by you), 15.3 (termination by us), 21 (changes to these Terms) and 22 (assignment);
c) wherever the law requires a refund.

Refunds are made with the payment method used for the payment, within 14 days unless another period is stated.

8.3. **Free plan and deletion.** You can stop using the Free plan at any time. To end the contract completely, you can ask us to delete your Account or your Organization (Section 17.3).

## 9. Consumers and Non-professionals

9.1. **Persons who are not professionals.** The Service is offered neither to Consumers nor to Non-professionals. If you are one of them, do not create an Account: you can instead run the Software yourself under the AGPL-3.0 (Section 11). If you have created an Account or subscribed nonetheless, or if you stop using the Service for professional purposes, tell us at romulus@lupidae.com. In that case:

a) if we have serious reasons to doubt that the Service is used for professional purposes, we may ask you for the information needed to check it, such as the name, SIREN or VAT number of the business, or the name of your employer. If you do not answer within 15 days, we may end the contract under b);
b) you can end the contract at any time, with immediate effect or from the date you choose, and we can end it by email with 30 days' notice. In both cases, you can export your Customer Content until the contract ends and for 30 days after (Section 15.4);
c) we refund the prepaid fees for the unused part of the current Billing Period and, if the contract ends less than 14 days after you first subscribed to a paid Plan, all the fees you paid for it. No late-payment interest or recovery indemnity (Section 7.9) is claimed.

9.2 to 9.6. Reserved.

9.7. **Mandatory law.** Nothing in these Terms limits the rights that a Consumer or a Non-professional who uses the Service despite Section 3.5 has under the mandatory provisions of French law or, where they apply, of the law of their country of residence.

## 10. Customer Content

10.1. **Ownership.** You keep all rights in your Customer Content. romulus claims no ownership of it.

10.2. **Licence to host.** You grant romulus a non-exclusive, free licence, for the whole world, for the duration of the contract and the retention periods set out in Section 17, to host, store, copy, transmit, display, analyse and transform (for example to format, compare and generate migrations from) your Customer Content, **solely** to provide, secure and support the Service for you and the members of your Organization, and to comply with the law. We do not use Customer Content for any other purpose, in particular not to train machine-learning models, and we do not sell it.

10.3. **Your responsibility.** You confirm that you have the rights needed to submit your Customer Content and that it does not infringe the law or third-party rights. Schema definitions and comments should not contain personal data unless necessary. Where they do, the Data Processing Agreement applies (Section 16).

10.4. **Confidentiality.** We treat Customer Content as confidential. Only the people who operate the Service for us, who are bound by confidentiality, can access it, and only when needed to operate, secure or support the Service, or at your request. Our providers process it only as described in the Privacy Policy.

## 11. Open source

11.1. The Software is free software, published under the AGPL-3.0 at https://github.com/lupidae/romulus. Nothing in these Terms restricts any right you have under the AGPL-3.0 to use, study, modify or distribute the Software, or to run it yourself.

11.2. These Terms apply only to the hosted Service, which includes hosting, storage, operations and support. A subscription buys access to the hosted Service, not a licence to the Software, which you already receive under the AGPL-3.0.

11.3. As required by Section 13 of the AGPL-3.0, the source code of the version of the Software that runs the Service is published at https://github.com/lupidae/romulus, and the Service shows which version it runs.

11.4. The AGPL-3.0 grants no right to use the names "romulus" and "lupidae" or their logos, which are reserved by the editor of the Service.

11.5. Third-party open-source components are governed by their own licences.

## 12. Feedback

If you send us suggestions or ideas about the Service or the Software ("**Feedback**"), you grant us, free of charge and on a non-exclusive basis, the right to reproduce, adapt, translate and integrate that Feedback, and to communicate it to the public, in the Service, in the Software and in their documentation, including in code published under the AGPL-3.0. This licence is granted for the whole world and for the duration of the legal protection of the Feedback. Feedback does not include your Customer Content. You remain free to use your own ideas.

## 13. Availability and support

13.1. **No service level.** We do our best to keep the Service available and to schedule maintenance to limit disruption, announcing planned interruptions when we can. **No Plan includes a service level agreement (SLA)** or a guaranteed availability, uptime or response time. The Service is not designed for high availability; keep your own exports of important data (Section 17.1).

13.2. **Third parties and force majeure.** The Service depends on third-party infrastructure (hosting, network, email, payment). We are not liable for a failure to perform caused by force majeure within the meaning of Article 1218 of the French Civil Code.

13.3. **Support.** Support is provided by email at romulus@lupidae.com on paid Plans, and through the community channels of the Software on the Free plan. We aim to answer within the times shown on the pricing page. Questions about your personal data, your contract, billing or security are always answered by email, whatever your Plan.

## 14. Suspension

14.1. We may suspend all or part of your access to the Service, including stopping Playgrounds, if:

a) you seriously breach Section 6 (Acceptable use);
b) your use creates a security risk or threatens the integrity or availability of the Service for other users;
c) a payment is overdue, as described in Section 7.8;
d) the law or a competent authority requires it.

We give prior notice by email stating the reasons, unless urgency or the law prevents it; in that case we inform you without delay afterwards.

14.2. A suspension is limited to what is necessary and lifted as soon as its cause has disappeared. Where the suspension is not caused by you, the fees for a paid Plan are not owed for the suspension period, and we refund or credit them.

## 15. Term and termination

15.1. **Duration.** The contract for the Free plan is concluded for an indefinite period. A paid subscription runs for the chosen Billing Period and renews as set out in Section 7.5.

15.2. **Termination by you.** You can end a paid subscription at any time (Section 8.1), and end the contract completely by asking us to delete your Account or Organization (Section 17.3) or by switching to another provider (Section 17.7).

15.3. **Termination by us.** We may terminate the contract:

a) without having to give a reason, by giving at least 30 days' notice by email; we then refund the prepaid fees for the unused part of the Billing Period;
b) if you seriously breach these Terms and do not remedy the breach within 15 days of a formal notice sent by email;
c) without prior notice, by an email stating the reasons, in case of a breach serious enough to require it, such as an attempt to escape a Playground's isolation, fraud or an attack on the Service (Article 1226 of the French Civil Code).

In cases b) and c), we refund the prepaid fees for the unused part of the Billing Period, after deducting any amount you owe us that is due.

15.4. **Effects of termination.** After a termination by us, you can export your Customer Content for 30 days, or ask us to send you the export, unless the law forbids it or the content is illegal. Your data is then deleted as described in Section 17.

15.5. **Survival.** Sections 10.3, 12, 17, 19, 20 and any other provision that by its nature must continue to apply survive the end of the contract.

## 16. Personal data

16.1. romulus processes, as controller, the personal data relating to Accounts, Organizations, billing, security and support, as described in the Privacy Policy at https://romulus.lupidae.com/legal/privacy.

16.2. Where Customer Content or the Organization's audit log contains personal data, romulus processes it on behalf of the Customer as a processor. The Data Processing Agreement at https://romulus.lupidae.com/legal/dpa forms part of these Terms.

16.3. Reserved.

## 17. Export, deletion and retention of data

17.1. **Export.** At any time and on every Plan, without a cap, you can export your Customer Content as a ZIP file through the Service's interface or API. A member can export a database; an administrator can export the whole Organization. The export contains, for each database, the SQL (DDL) of every version, including versions hidden by the history cap, the generated migrations (up and down), drafts, comments and the layout and description files (JSON). It does not contain the audit log, the member list, Playground data or invoices; invoices are available in the billing portal, and the audit log and the member list are provided on request (Section 17.7).

17.2. **Cancellation.** Cancelling a paid Plan deletes nothing: the Organization moves to the Free plan and you can keep using and exporting it.

17.3. **Deleting an Account or an Organization.** You can ask us to delete your Account, or an administrator can ask us to delete the Organization, by writing to romulus@lupidae.com from the email address of the Account. We do it within 30 days and confirm it by email. Export your data before asking. If you are the last administrator of an Organization that has other members, appoint another administrator first, or ask for the Organization to be deleted too. If an Account has not been used for 3 years, we delete it after an email notice sent at least 30 days in advance; if it was the only member of an Organization on the Free plan, that Organization is deleted with it.

17.4. **Deleted databases and removed versions.** A database deleted by an administrator stops being accessible at once and is permanently deleted after 30 days. A version removed by an administrator can be restored for 30 days, then it is permanently deleted, or, if an open draft is based on it, when that draft is closed.

17.5. **Backups.** We back up the Service's databases every day and keep each backup for 14 days. Deleted data may therefore remain in backups for up to 30 days after its deletion. Backups are used only to restore the Service.

17.6. **Data kept by law.** We keep invoices and accounting records for 10 years (Article L123-22 of the French Commercial Code), and records of acceptance of these Terms as stated in Section 3.6.

17.7. **Switching to another provider** (Articles 23 to 29 of the Data Act). You can switch at any time, free of charge, to a service offered by another provider or to your own infrastructure, for example by running the Software yourself (Section 11).

a) **Request.** You start switching by writing to romulus@lupidae.com from the address of an administrator, stating whether you are switching to another provider (name it if you want us to work with it), switching to your own infrastructure, or only asking for your data to be erased. We require no notice period: the process starts when we receive your request. Your paid subscription is then no longer renewed, unless you ask otherwise.

b) **Transitional period.** For 30 days from your request, or for a longer period you choose, which you can extend once, the contract remains in force. During that period, we give you and the third parties you authorize reasonable assistance and all the information relevant to your exit, we continue to provide the Service with due care, we inform you of any known risk to its continuity, and we maintain a high level of security for your data, including while it is transferred.

c) **Exportable data.** You can take: your Customer Content, in the export described in Section 17.1; and, on request, the audit log of your Organizations, the list of their members and roles, and the text of the Playground queries and migrations that we still hold (Section 6.3). Playgrounds, which contain only generated fake data and are deleted under Section 6.3, are not exported. Technical and security logs, which concern only the operation and security of the Service, are not part of the exportable data.

d) **Formats.** The export uses open formats: PostgreSQL SQL and JSON files, in a ZIP archive (Section 4.4). We publish, and keep up to date, a description of its structure and formats in the documentation of the Software at https://github.com/lupidae/romulus.

e) **Retrieval period.** After the transitional period, you can still retrieve your exportable data for 30 days.

f) **End of the contract and erasure.** The contract ends, for the Organizations concerned, when you confirm that the switch is complete or, at the latest, at the end of the retrieval period; if you only asked for erasure, it ends when we receive your request. We confirm the end by email. We then erase all the exportable data of those Organizations within 30 days, subject to Section 17.5 (backups) and Section 17.6 (data kept by law).

g) **Charges.** Switching is free: we charge no fee for switching, exporting or transferring data, and no early termination penalty. Ending the contract does not, however, entitle you to a refund of the fees already paid for the current Billing Period, except in the cases of Section 8.2.

## 18. Warranties

18.1. We provide the Service with the care and diligence of a professional, under an obligation of means (*obligation de moyens*).

18.2. We do not guarantee that the Service will be uninterrupted or free from errors, or that generated migrations are free from errors or suitable for your databases.

## 19. Liability

19.1. **Limits.** To the extent permitted by law:

a) romulus is liable only for direct and foreseeable damage resulting from a fault of romulus that you prove;
b) romulus is not liable for loss of profit, revenue, business or goodwill, or for damage resulting from the application to your own databases of a migration that you reviewed and decided to apply;
c) romulus's total liability, for all claims together, is limited to the higher of **the amounts you paid for the Service during the 12 months before the event giving rise to the first claim** and **EUR 500**, or to **EUR 100** if you have never paid for a Plan;
d) these limitations do not apply in case of gross negligence (*faute lourde*) or wilful misconduct (*faute dolosive*), to personal injury, or to any liability that cannot be limited under French law.

19.2. Reserved.

19.3. **Your copies.** We recommend that you keep your own exports of important schemas and that you review migrations before applying them.

## 20. Governing law and disputes

20.1. These Terms are governed by **French law**.

20.2. **Disputes.** The parties will first try to settle any dispute amicably, for 30 days from a written notice. Failing that, where both parties have contracted as merchants (*commerçants*), the competent courts of Lille (France) have exclusive jurisdiction, including in case of multiple defendants or third-party claims. The same courts have jurisdiction over Customers established in another Member State of the European Union, under Article 25 of Regulation (EU) No 1215/2012. In other cases, the ordinary rules of jurisdiction apply.

20.3. Reserved.

## 21. Changes to these Terms

21.1. We may change these Terms, for example to reflect a change in the law, in the Service, or in our organization, including an assignment under Section 22. Each version is dated and numbered.

21.2. **Notice.** We tell you about any change by email at least 30 days before it takes effect, and show the new Terms in the Service. Changes required by law or by an authority can take effect sooner, if the law so requires.

21.3. **Your choice.** If you do not accept the change, you can terminate the contract free of charge before the effective date, and receive a refund of the prepaid fees for the unused part of the Billing Period. If you continue to use the Service after the effective date, the new Terms apply to you.

21.4. **Limits.** A change under this Section never modifies, for a Billing Period already paid, its price, its duration or the features and caps of your Plan, except for changes to the Service made under Section 4.5. A new price applies only at renewal, under Section 7.6.

## 22. Assignment and successor company

22.1. romulus is currently operated by Lucas Lefort as an entrepreneur individuel. The business is intended to be transferred to a company to be formed, expected to be a French *société par actions simplifiée* (SAS) controlled by Lucas Lefort (the "**Successor**").

22.2. **Conditions of any assignment.** romulus may assign this contract, with all rights and obligations under it and the associated data, only to the Successor or to an entity that takes over all or substantially all of the business of the Service, and only if the assignee continues the contract on the same terms. We tell you by email at least 30 days before the assignment takes effect, identifying the assignee. The assignment takes effect towards you on the date stated in that email.

22.3. **Your consent.** You agree in advance to such an assignment, under Article 1216 of the French Civil Code, and expressly agree that it releases the assignor for the future (Article 1216-1).

22.4. **Your right to terminate.** The assignment does not reduce your rights under these Terms. You can terminate the contract free of charge at any time from the notice until three months after the assignment takes effect, and receive a refund of the prepaid fees for the unused part of the Billing Period.

22.5. You may not assign this contract without our prior written consent. You may however assign it to the successor of your business, by notifying us.

## 23. General provisions

23.1. **Contract documents.** The contract consists of these Terms, the description of the Plans in Section 7.1 and on the pricing page, and the Data Processing Agreement. In case of conflict, the Data Processing Agreement prevails on personal data, then these Terms. The Privacy Policy informs you about our processing of personal data. Your general terms of purchase do not apply.

23.2. **Severability.** If any provision is held invalid, the other provisions remain in force.

23.3. **No waiver.** Failure to enforce a provision at a given time is not a waiver of the right to enforce it later.

23.4. **Electronic records and proof.** Between romulus and the Customer, the electronic records kept by romulus and by Stripe are evidence of the orders, acceptances, attestations and payments, unless proven otherwise.

23.5. **Notices.** We send notices to the email address of your Account or, for matters concerning an Organization, to its administrators. You send notices to romulus@lupidae.com or to the postal address in Section 1.

23.6. **Independence.** The parties are independent contractors. Nothing in these Terms creates a partnership, an agency or an employment relationship.

## 24. Contact

- Lucas Lefort EI (romulus), 108 rue de Gand, 59200 Tourcoing, France
- Email, for any question (support, personal data, legal, abuse reports, security, switching): **romulus@lupidae.com**
- Telephone: +33 7 82 98 35 07
