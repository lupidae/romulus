// The words of the hosted service's billing, in English and in French (the
// French version of the Terms prevails for consumers in France): the checkout
// dialog, the billing page, withdrawal and cancellation. Terms of Service
// v1.0, sections 3.4, 3.5, 7, 8, 9.3 and 9.4; French Consumer Code L221-5,
// L221-14, L221-18, L221-21, L221-25 and L221-28.
//
// What a box says is what the buyer states: those sentences live in
// checkout-boxes.json, which the API reads too (it records the version ticked
// with the acceptance and quotes the boxes in the confirmation email). The
// checks below must read exactly as that file says (copy.test.ts).

import type { Locale } from "@/features/legal/locale";

import boxes from "./checkout-boxes.json";

/**
 * The version of each edition's boxes: the API answers the one in force
 * (`LegalLinks.copy_version`), which a request sends back with what was ticked.
 */
export const copyVersions = {
  professional: boxes.professional.version,
  full: boxes.full.version,
} as const;

export interface CheckoutCopy {
  title: (plan: string) => string;
  buyerLegend: string;
  buyerMissing: string;
  business: { label: string; description: string };
  consumer: { label: string; description: string };
  companyName: string;
  companyNameMissing: string;
  country: string;
  countryPlaceholder: string;
  countryMissing: string;
  countryNone: string;
  taxId: string;
  taxIdHint: string;
  termsBefore: string;
  termsLink: string;
  termsAfter: (version: string) => string;
  privacyBefore: string;
  privacyLink: string;
  termsMissing: string;
  /** The business box: before the DPA's link, the link, after it. */
  businessBefore: string;
  businessLink: string;
  businessAfter: string;
  businessMissing: string;
  /** The professional edition (businesses only): line above the boxes, its box, its words. */
  pro: {
    intro: string;
    businessBefore: string;
    businessLink: string;
    businessAfter: string;
    missing: string;
    next: string;
  };
  immediateStart: string;
  withdrawal: string;
  consumerMissing: string;
  next: string;
  commitment: (interval: "month" | "year") => string;
  submit: string;
  cancel: string;
  newTab: string;
  unreachable: string;
  copyChanged: string;
}

export const checkoutCopy: Record<Locale, CheckoutCopy> = {
  en: {
    title: (plan) => `Upgrade to ${plan}`,
    buyerLegend: "You are buying as",
    buyerMissing: "Choose whether you are buying as a business or as an individual.",
    business: {
      label: "A business",
      description: "A company, a sole trader or a professional, for your work.",
    },
    consumer: {
      label: "An individual",
      description: "For personal use, as a consumer. You have a 14-day right of withdrawal.",
    },
    companyName: "Business name",
    companyNameMissing: "Enter your business's name.",
    country: "Country",
    countryPlaceholder: "Search for a country",
    countryMissing: "Choose your business's country.",
    countryNone: "No country by that name",
    taxId: "VAT or SIREN number",
    taxIdHint:
      "Optional: it helps show you buy for your business. To have it on your invoices, also enter your VAT number on the payment page.",
    termsBefore: "I have read and accept the",
    termsLink: "Terms of Service",
    termsAfter: (version) => `(version ${version}).`,
    privacyBefore: "How we handle your personal data:",
    privacyLink: "Privacy Policy",
    termsMissing: "Accept the Terms of Service to continue.",
    businessBefore:
      "I am subscribing for the purposes of my trade, business or profession, on behalf of the business named on the payment page, which I am authorised to bind. The",
    businessLink: "Data Processing Agreement",
    businessAfter: "applies.",
    businessMissing: "Confirm that you are buying for your business.",
    pro: {
      intro:
        "For professional use only. On the payment page, add the name of your business and, if it has one, its VAT number.",
      businessBefore:
        "I am subscribing for the purposes of my trade, business or profession, on behalf of the business named on the payment page, which I am authorised to bind. I am not subscribing as a consumer. The",
      businessLink: "Data Processing Agreement",
      businessAfter: "applies.",
      missing: "Confirm that you are subscribing for your business.",
      next: 'You will enter your payment details on Stripe\'s secure page. Clicking the payment button on that page ("Subscribe" or "Pay and subscribe") places an order with an obligation to pay. Prices in euros, excluding taxes; VAT not applicable (art. 293 B du CGI).',
    },
    immediateStart: boxes.full.en.immediate_start,
    withdrawal: boxes.full.en.withdrawal,
    consumerMissing:
      "To subscribe now, tick both boxes. If you would rather not, you can stay on the Free plan.",
    next: 'You will enter your payment details on Stripe\'s secure page. Clicking the payment button on that page ("Subscribe" or "Pay and subscribe") places an order with an obligation to pay. Prices are in euros, VAT not applicable (art. 293 B du CGI).',
    commitment: (interval) =>
      interval === "month"
        ? "renewed every month until you cancel. Cancel any time in Billing: the plan stays until the end of the month."
        : "renewed every year until you cancel. We remind you about 40 days before each renewal. Cancel any time in Billing: the plan stays until the end of the year.",
    submit: "Continue to payment",
    cancel: "Cancel",
    newTab: "(opens in a new tab)",
    unreachable: "Could not reach billing. Try again.",
    copyChanged: "The wording of this page changed: reload it, then read the boxes again.",
  },
  fr: {
    title: (plan) => `Passer à l'offre ${plan}`,
    buyerLegend: "Vous achetez en tant que",
    buyerMissing: "Indiquez si vous achetez en tant que professionnel ou en tant que particulier.",
    business: {
      label: "Professionnel",
      description:
        "Une société, un entrepreneur individuel ou un professionnel, pour votre activité.",
    },
    consumer: {
      label: "Particulier",
      description:
        "Pour un usage personnel, en tant que consommateur. Vous disposez d'un droit de rétractation de 14 jours.",
    },
    companyName: "Nom ou dénomination de l'entreprise",
    companyNameMissing: "Indiquez le nom ou la dénomination de l'entreprise.",
    country: "Pays",
    countryPlaceholder: "Rechercher un pays",
    countryMissing: "Choisissez le pays de l'entreprise.",
    countryNone: "Aucun pays de ce nom",
    taxId: "N° de TVA ou SIREN",
    taxIdHint:
      "Facultatif : il atteste votre achat professionnel. Pour qu'il figure sur vos factures, indiquez aussi votre numéro de TVA sur la page de paiement.",
    termsBefore: "J'ai lu et j'accepte les",
    termsLink: "Conditions générales d'utilisation et de vente",
    termsAfter: (version) => `(version ${version}).`,
    privacyBefore: "Le traitement de vos données personnelles est décrit dans notre",
    privacyLink: "Politique de confidentialité",
    termsMissing: "Acceptez les Conditions générales pour continuer.",
    businessBefore:
      "Je souscris pour les besoins de mon activité professionnelle, au nom de l'entreprise indiquée sur la page de paiement, que je suis habilité(e) à engager. L'",
    businessLink: "accord de traitement des données",
    businessAfter: "s'applique.",
    businessMissing: "Confirmez que vous achetez pour votre activité professionnelle.",
    pro: {
      intro:
        "Réservé aux professionnels. Sur la page de paiement, indiquez la dénomination de votre entreprise et, le cas échéant, son numéro de TVA.",
      businessBefore:
        "Je souscris pour les besoins de mon activité professionnelle, au nom de l'entreprise indiquée sur la page de paiement, que je suis habilité(e) à engager. Je ne souscris pas en tant que consommateur. L'",
      businessLink: "accord de traitement des données",
      businessAfter: "s'applique.",
      missing: "Confirmez que vous souscrivez pour votre activité professionnelle.",
      next: "Vous saisirez vos informations de paiement sur la page sécurisée de Stripe. Le clic sur le bouton de paiement de cette page (« S'abonner » ou « Payer et s'abonner ») vaut commande avec obligation de paiement. Prix en euros hors taxes ; TVA non applicable (art. 293 B du CGI).",
    },
    immediateStart: boxes.full.fr.immediate_start,
    withdrawal: boxes.full.fr.withdrawal,
    consumerMissing:
      "Pour souscrire maintenant, cochez les deux cases. Sinon, vous pouvez rester sur l'offre gratuite (Free).",
    next: "Vous saisirez vos informations de paiement sur la page sécurisée de Stripe. Le clic sur le bouton de paiement de cette page (« S'abonner » ou « Payer et s'abonner ») vaut commande avec obligation de paiement. Prix en euros, TVA non applicable (art. 293 B du CGI).",
    commitment: (interval) =>
      interval === "month"
        ? "renouvelé chaque mois jusqu'à résiliation. Résiliable à tout moment dans la facturation : l'offre reste active jusqu'à la fin du mois."
        : "renouvelé chaque année jusqu'à résiliation. Nous vous le rappelons environ 40 jours avant chaque renouvellement. Résiliable à tout moment dans la facturation : l'offre reste active jusqu'à la fin de l'année.",
    submit: "Continuer vers le paiement",
    cancel: "Annuler",
    newTab: "(s'ouvre dans un nouvel onglet)",
    unreachable: "Impossible de joindre la facturation. Réessayez.",
    copyChanged: "Le texte de cette page a changé : rechargez-la, puis relisez les cases.",
  },
};

export type Buyer = "business" | "consumer";

/**
 * What a checkout may be sent with: a buyer chosen (never assumed), the Terms,
 * a business's statement, name and country, or a consumer's two requests.
 */
export function checkoutProblems(
  form: {
    /** Businesses only (the professional edition): its own box and words. */
    pro?: boolean;
    buyer: Buyer | undefined;
    acceptTerms: boolean;
    businessConfirmed: boolean;
    companyName: string;
    country: string | undefined;
    immediateStart: boolean;
    withdrawal: boolean;
  },
  locale: Locale = "en",
): {
  buyer?: string;
  terms?: string;
  business?: string;
  companyName?: string;
  country?: string;
  consumer?: string;
} {
  const copy = checkoutCopy[locale];
  const business = form.buyer === "business";
  return {
    buyer: form.buyer ? undefined : copy.buyerMissing,
    terms: form.acceptTerms ? undefined : copy.termsMissing,
    business:
      business && !form.businessConfirmed
        ? form.pro
          ? copy.pro.missing
          : copy.businessMissing
        : undefined,
    companyName: business && !form.companyName.trim() ? copy.companyNameMissing : undefined,
    country: business && !form.country ? copy.countryMissing : undefined,
    consumer:
      form.buyer === "consumer" && !(form.immediateStart && form.withdrawal)
        ? copy.consumerMissing
        : undefined,
  };
}

/** The billing page, withdrawal and cancellation. */
export interface BillingCopy {
  page: {
    title: string;
    description: string;
    paid: string;
    paidBody: string;
    checkoutCancelled: string;
    nothingCharged: string;
    noBilling: string;
    noBillingBody: string;
    loadFailed: string;
    retry: string;
    nothingToCancel: string;
    nothingToCancelBody: (org: string, contact: string) => string;
    notAdmin: (org: string) => string;
  };
  summary: {
    title: string;
    onHold: string;
    nothingToPay: string;
    price: (amount: string, interval: "month" | "year") => string;
    ends: (date: string) => string;
    renews: (date: string) => string;
    switches: (plan: string, interval: "month" | "year" | null, date: string) => string;
    overdue: string;
    overdueBody: (plan: string, date: string) => string;
    updatePayment: string;
    renewHint: string;
    cancelHint: string;
    cancel: string;
    manage: string;
    adminsOnly: string;
    portalFailed: string;
    checkConnection: string;
  };
  status: {
    cancels: string;
    active: string;
    overdue: string;
    cancelled: string;
    inactive: string;
  };
  withdrawal: {
    title: string;
    body: (date: string) => string;
    button: string;
    confirmTitle: (plan: string) => string;
    confirmBody: (org: string) => string;
    confirm: string;
    keep: string;
    done: string;
    doneBody: (refund: string) => string;
    failed: string;
    withdrawnTitle: (when: string) => string;
    withdrawnBody: (refund: string, completed: boolean) => string;
  };
  usage: {
    title: string;
    against: (plan: string) => string;
    members: string;
    databases: string;
    versions: string;
    mostIn: (db: string) => string;
    schema: string;
    largest: (db: string) => string;
    history: string;
    hidden: (n: number) => string;
    hours: string;
    hoursWait: (date: string) => string;
    hoursReset: (date: string) => string;
    playground: string;
    size: (size: "small" | "medium") => string;
    approvals: string;
    approvalsNone: string;
    approvalsValue: (used: number, limit: number, override: boolean) => string;
    near: string;
    full: string;
    over: string;
    of: (used: string, limit: string) => string;
    unlimited: (used: string) => string;
  };
  plans: {
    title: string;
    period: string;
    monthly: string;
    yearly: string;
    upgrade: (plan: string) => string;
    downgrade: (plan: string) => string;
    switchTo: (interval: "month" | "year") => string;
    fromRenewal: string;
    chargedNow: string;
    afterCancel: string;
    current: string;
    forever: string;
    per: (interval: "month" | "year") => string;
    footer: string;
    /** Businesses only: prices excluding taxes (Terms 7.3, professional edition). */
    footerPro: string;
  };
  invoices: {
    title: string;
    description: string;
    loadFailed: string;
    empty: string;
    emptyBody: string;
    date: string;
    number: string;
    amount: string;
    status: string;
    open: string;
    view: string;
    viewLabel: (number: string) => string;
    paid: string;
    due: string;
    unpaid: string;
    void: string;
  };
  contract: {
    cancelYourContract: string;
  };
}

export const billingCopy: Record<Locale, BillingCopy> = {
  en: {
    page: {
      title: "Billing",
      description: "Your plan, what it allows and what you use. Cancel any time.",
      paid: "Payment received",
      paidBody: "Your plan shows here as soon as Stripe confirms it.",
      checkoutCancelled: "Checkout cancelled",
      nothingCharged: "Nothing was charged.",
      noBilling: "No billing on this server",
      noBillingBody: "This server has no plans: nothing is capped, and there is nothing to pay.",
      loadFailed: "Could not load billing",
      retry: "Retry",
      nothingToCancel: "No paid plan to cancel",
      nothingToCancelBody: (org, contact) =>
        `${org} is on the Free plan: nothing is charged and nothing renews. To end your contract completely, ask us to delete your account or organization by writing to ${contact}.`,
      notAdmin: (org) => `Only the admins of ${org} can cancel its subscription.`,
    },
    summary: {
      title: "Plan",
      onHold: "Paid plan on hold",
      nothingToPay: "Nothing to pay. Choose a plan below to raise its limits.",
      price: (amount, interval) => `${amount} a ${interval}`,
      ends: (date) => `Ends on ${date}, then the organization is on Free. Nothing is deleted.`,
      renews: (date) => `Renews on ${date}`,
      switches: (plan, interval, date) =>
        `Switches to ${plan}${interval ? ` (${interval === "year" ? "yearly" : "monthly"})` : ""} on ${date}.`,
      overdue: "Payment overdue",
      overdueBody: (plan, date) =>
        `The ${plan} plan holds until ${date}. After that the organization moves to Free: nothing is deleted, but creating past Free's caps waits.`,
      updatePayment: "Update payment method",
      renewHint: "To keep the plan after that date, renew it from Manage billing.",
      cancelHint: "Cancelling keeps the plan until the end of the period you paid for.",
      cancel: "Cancel subscription",
      manage: "Manage billing",
      adminsOnly: "Only admins change the plan and see invoices.",
      portalFailed: "Could not open billing",
      checkConnection: "Check your connection.",
    },
    status: {
      cancels: "Cancels at period end",
      active: "Active",
      overdue: "Payment overdue",
      cancelled: "Cancelled",
      inactive: "Not active",
    },
    withdrawal: {
      title: "Right of withdrawal",
      body: (date) =>
        `You subscribed as a consumer: until ${date}, you can withdraw from the contract. You then pay only for the days used, the rest is refunded within 14 days, and the organization moves to the Free plan. Nothing is deleted.`,
      button: "Withdraw from contract here",
      confirmTitle: (plan) => `Withdraw from the ${plan} plan?`,
      confirmBody: (org) =>
        `The subscription ends now and ${org} moves to the Free plan; nothing is deleted. We refund the days not used to your payment method within 14 days, and email you an acknowledgement.`,
      confirm: "Confirm withdrawal",
      keep: "Keep the plan",
      done: "Withdrawal received",
      doneBody: (refund) =>
        `We emailed you an acknowledgement. ${refund} is refunded within 14 days.`,
      failed: "Could not withdraw",
      withdrawnTitle: (when) => `Withdrawal received on ${when}`,
      withdrawnBody: (refund, completed) =>
        completed
          ? `${refund} was refunded to your payment method; it shows on your statement within days. The organization is on the Free plan, and nothing was deleted.`
          : `${refund} is being refunded to your payment method, within 14 days. The organization is on the Free plan, and nothing was deleted.`,
    },
    usage: {
      title: "Usage",
      against: (plan) =>
        `Against the ${plan} plan. Reaching a limit stops creating more; nothing is deleted.`,
      members: "Members and invitations",
      databases: "Databases",
      versions: "Versions per database",
      mostIn: (db) => `Most in ${db}`,
      schema: "Tables and views per schema version",
      largest: (db) => `Largest: ${db}`,
      history: "History shown",
      hidden: (n) =>
        `${n.toLocaleString("en-US")} ${n === 1 ? "older version is" : "older versions are"} kept, hidden by the plan`,
      hours: "Playground hours this month",
      hoursWait: (date) => `Playgrounds and tests wait until ${date}`,
      hoursReset: (date) => `Counts again from ${date}`,
      playground: "Largest playground",
      size: (size) => (size === "small" ? "Small rows" : "Medium rows"),
      approvals: "Required approvals",
      approvalsNone: "Not in this plan",
      approvalsValue: (used, limit, override) =>
        `${used} required, up to ${limit}${override ? ", admin override" : ""}`,
      near: "Almost at the limit",
      full: "At the limit",
      over: "Over the limit: creating more waits",
      of: (used, limit) => `${used} of ${limit}`,
      unlimited: (used) => `${used} (no limit)`,
    },
    plans: {
      title: "Plans",
      period: "Billing period",
      monthly: "Monthly",
      yearly: "Yearly, 2 months free",
      upgrade: (plan) => `Upgrade to ${plan}`,
      downgrade: (plan) => `Downgrade to ${plan}`,
      switchTo: (interval) => `Switch to ${interval === "year" ? "yearly" : "monthly"}`,
      fromRenewal: "From your next renewal.",
      chargedNow: "Charged now, prorated.",
      afterCancel: "What the organization keeps after cancelling.",
      current: "Current plan",
      forever: "forever",
      per: (interval) => `a ${interval}`,
      footer:
        "Prices in euros, VAT not applicable (art. 293 B du CGI). Upgrades apply at once, prorated by Stripe; downgrades at the end of the period. Cancel any time.",
      footerPro:
        "For professional use only. Prices in euros, excluding taxes; VAT not applicable (art. 293 B du CGI). Upgrades apply at once, prorated by Stripe; downgrades at the end of the period. Cancel any time.",
    },
    invoices: {
      title: "Invoices",
      description: "Each one opens on Stripe, to read, download or pay.",
      loadFailed: "Could not load the invoices",
      empty: "No invoices yet",
      emptyBody: "Each payment's invoice appears here, starting with the first plan you choose.",
      date: "Date",
      number: "Number",
      amount: "Amount",
      status: "Status",
      open: "Open",
      view: "View",
      viewLabel: (number) => `invoice ${number} (opens in a new tab)`,
      paid: "Paid",
      due: "Due",
      unpaid: "Unpaid",
      void: "Void",
    },
    contract: {
      cancelYourContract: "Cancel your contract",
    },
  },
  fr: {
    page: {
      title: "Facturation",
      description:
        "Votre offre, ce qu'elle permet et ce que vous utilisez. Résiliable à tout moment.",
      paid: "Paiement reçu",
      paidBody: "Votre offre s'affiche ici dès que Stripe la confirme.",
      checkoutCancelled: "Paiement annulé",
      nothingCharged: "Rien n'a été prélevé.",
      noBilling: "Pas de facturation sur ce serveur",
      noBillingBody: "Ce serveur n'a pas d'offres : rien n'est limité, et il n'y a rien à payer.",
      loadFailed: "Impossible de charger la facturation",
      retry: "Réessayer",
      nothingToCancel: "Aucune offre payante à résilier",
      nothingToCancelBody: (org, contact) =>
        `${org} est sur l'offre gratuite (Free) : rien n'est prélevé et rien n'est reconduit. Pour mettre fin entièrement au contrat, demandez-nous la suppression de votre compte ou de votre organisation en écrivant à ${contact}.`,
      notAdmin: (org) => `Seuls les administrateurs de ${org} peuvent résilier son abonnement.`,
    },
    summary: {
      title: "Offre",
      onHold: "Offre payante suspendue",
      nothingToPay: "Rien à payer. Choisissez une offre ci-dessous pour relever ses limites.",
      price: (amount, interval) => `${amount} par ${interval === "month" ? "mois" : "an"}`,
      ends: (date) =>
        `Prend fin le ${date}, puis l'organisation passe à l'offre gratuite (Free). Rien n'est supprimé.`,
      renews: (date) => `Reconduite le ${date}`,
      switches: (plan, interval, date) =>
        `Passe à l'offre ${plan}${interval ? ` (${interval === "year" ? "annuelle" : "mensuelle"})` : ""} le ${date}.`,
      overdue: "Paiement en retard",
      overdueBody: (plan, date) =>
        `L'offre ${plan} reste active jusqu'au ${date}. Ensuite, l'organisation passe à l'offre gratuite (Free) : rien n'est supprimé, mais créer au-delà de ses limites attend.`,
      updatePayment: "Mettre à jour le moyen de paiement",
      renewHint: "Pour garder l'offre après cette date, reprenez-la depuis Gérer la facturation.",
      cancelHint: "Résilier garde l'offre jusqu'à la fin de la période payée.",
      cancel: "Résilier l'abonnement",
      manage: "Gérer la facturation",
      adminsOnly: "Seuls les administrateurs changent l'offre et voient les factures.",
      portalFailed: "Impossible d'ouvrir la facturation",
      checkConnection: "Vérifiez votre connexion.",
    },
    status: {
      cancels: "Prend fin en fin de période",
      active: "Active",
      overdue: "Paiement en retard",
      cancelled: "Résiliée",
      inactive: "Inactive",
    },
    withdrawal: {
      title: "Droit de rétractation",
      body: (date) =>
        `Vous avez souscrit en tant que consommateur : jusqu'au ${date}, vous pouvez vous rétracter du contrat. Vous ne payez alors que les jours utilisés, le reste est remboursé sous 14 jours, et l'organisation passe à l'offre gratuite (Free). Rien n'est supprimé.`,
      button: "Renoncer au contrat ici",
      confirmTitle: (plan) => `Vous rétracter de l'offre ${plan} ?`,
      confirmBody: (org) =>
        `L'abonnement prend fin maintenant et ${org} passe à l'offre gratuite (Free) ; rien n'est supprimé. Nous remboursons les jours non utilisés sur votre moyen de paiement sous 14 jours, et vous envoyons un accusé de réception par e-mail.`,
      confirm: "Confirmer la rétractation",
      keep: "Garder l'offre",
      done: "Rétractation reçue",
      doneBody: (refund) =>
        `Nous vous avons envoyé un accusé de réception par e-mail. ${refund} vous seront remboursés sous 14 jours.`,
      failed: "Impossible de se rétracter",
      withdrawnTitle: (when) => `Rétractation reçue le ${when}`,
      withdrawnBody: (refund, completed) =>
        completed
          ? `${refund} ont été remboursés sur votre moyen de paiement ; ils apparaîtront sur votre relevé sous quelques jours. L'organisation est sur l'offre gratuite (Free), et rien n'a été supprimé.`
          : `${refund} sont en cours de remboursement sur votre moyen de paiement, sous 14 jours. L'organisation est sur l'offre gratuite (Free), et rien n'a été supprimé.`,
    },
    usage: {
      title: "Consommation",
      against: (plan) =>
        `Selon l'offre ${plan}. Atteindre une limite empêche d'en créer davantage ; rien n'est supprimé.`,
      members: "Membres et invitations",
      databases: "Bases de données",
      versions: "Versions par base de données",
      mostIn: (db) => `Le plus dans ${db}`,
      schema: "Tables et vues par version du schéma",
      largest: (db) => `La plus grande : ${db}`,
      history: "Historique affiché",
      hidden: (n) =>
        n === 1
          ? "1 version plus ancienne conservée, masquée par l'offre"
          : `${n.toLocaleString("fr-FR")} versions plus anciennes conservées, masquées par l'offre`,
      hours: "Heures de playground ce mois-ci",
      hoursWait: (date) => `Les playgrounds et les tests attendent le ${date}`,
      hoursReset: (date) => `Le compteur repart le ${date}`,
      playground: "Plus grand playground",
      size: (size) => (size === "small" ? "Petit volume" : "Volume moyen"),
      approvals: "Approbations requises",
      approvalsNone: "Pas dans cette offre",
      approvalsValue: (used, limit, override) =>
        `${used} requise${used > 1 ? "s" : ""}, jusqu'à ${limit}${override ? ", contournement par un administrateur" : ""}`,
      near: "Presque à la limite",
      full: "À la limite",
      over: "Au-delà de la limite : créer davantage attend",
      of: (used, limit) => `${used} sur ${limit}`,
      unlimited: (used) => `${used} (sans limite)`,
    },
    plans: {
      title: "Offres",
      period: "Période de facturation",
      monthly: "Mensuelle",
      yearly: "Annuelle, 2 mois offerts",
      upgrade: (plan) => `Passer à l'offre ${plan}`,
      downgrade: (plan) => `Passer à l'offre ${plan}`,
      switchTo: (interval) => `Passer en ${interval === "year" ? "annuel" : "mensuel"}`,
      fromRenewal: "À partir de votre prochaine reconduction.",
      chargedNow: "Facturé maintenant, au prorata.",
      afterCancel: "Ce que garde l'organisation après résiliation.",
      current: "Offre actuelle",
      forever: "pour toujours",
      per: (interval) => (interval === "month" ? "par mois" : "par an"),
      footer:
        "Prix en euros, TVA non applicable (art. 293 B du CGI). Un passage à une offre supérieure s'applique aussitôt, au prorata calculé par Stripe ; une offre inférieure, à la fin de la période. Résiliable à tout moment.",
      footerPro:
        "Réservé aux professionnels. Prix en euros hors taxes ; TVA non applicable (art. 293 B du CGI). Un passage à une offre supérieure s'applique aussitôt, au prorata calculé par Stripe ; une offre inférieure, à la fin de la période. Résiliable à tout moment.",
    },
    invoices: {
      title: "Factures",
      description: "Chacune s'ouvre sur Stripe, pour la lire, la télécharger ou la payer.",
      loadFailed: "Impossible de charger les factures",
      empty: "Aucune facture pour l'instant",
      emptyBody: "La facture de chaque paiement apparaît ici, dès la première offre choisie.",
      date: "Date",
      number: "Numéro",
      amount: "Montant",
      status: "Statut",
      open: "Ouvrir",
      view: "Voir",
      viewLabel: (number) => `la facture ${number} (s'ouvre dans un nouvel onglet)`,
      paid: "Payée",
      due: "À payer",
      unpaid: "Impayée",
      void: "Annulée",
    },
    contract: {
      cancelYourContract: "Résilier votre contrat",
    },
  },
};
