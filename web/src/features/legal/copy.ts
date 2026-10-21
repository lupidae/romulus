// The words of the Terms screen at sign-in (Terms 3.1 and 3.4 b: the boxes
// ticked there conclude the Free plan's contract; 21.3 and 21.4: a new
// version), in English and in French. While romulus is sold to professionals
// only (the professional edition), the screen also has the professional-use
// box, and a new version follows the professional edition's rule (21.3). The
// boxes say what checkout-boxes.json says (copy.test.ts checks it).

import boxes from "@/features/billing/checkout-boxes.json";

import type { Locale } from "./locale";

export interface TermsCopy {
  firstTitle: string;
  firstBody: string;
  /** The professional edition: title, line above the boxes, the box, help, button. */
  proTitle: string;
  proIntro: string;
  professionalUse: string;
  personalBefore: string;
  personalLink: string;
  personalAfter: string;
  proMissing: string;
  proSubmit: string;
  changedTitle: string;
  changedBody: (version: string, accepted: string) => string;
  changedRefuse: (contact: string) => string;
  /** The professional edition's rule for a new version (Terms 21.2, 21.3). */
  proChangedBody: (version: string, accepted: string) => string;
  proChangedRefuse: (contact: string) => string;
  termsBefore: string;
  termsLink: string;
  termsAfter: (version: string) => string;
  privacyBefore: string;
  privacyLink: string;
  newTab: string;
  missing: string;
  submit: string;
  later: string;
  signOut: string;
  language: string;
  versionChanged: string;
  failed: string;
  signOutFailed: string;
}

export const termsCopy: Record<Locale, TermsCopy> = {
  en: {
    firstTitle: "Accept the Terms of Service",
    firstBody:
      "To use romulus, on the Free plan or on a plan you buy later, accept its Terms of Service. They are the contract between you and romulus.",
    proTitle: "Before you continue",
    proIntro: "romulus is for professional use only.",
    professionalUse: boxes.professional.en.professional_use,
    personalBefore: "For a personal project, you can run the",
    personalLink: "open-source software",
    personalAfter: "yourself.",
    proMissing: "Confirm that you will use romulus for professional purposes.",
    proSubmit: "Continue",
    changedTitle: "The Terms of Service changed",
    changedBody: (version, accepted) =>
      `Version ${version} is in force. Read it, then accept it to keep using romulus. Until you do, the version you accepted (${accepted}) applies until the end of your billing period.`,
    changedRefuse: (contact) =>
      `If you do not accept the new version, you can end your contract at no cost: cancel your subscription in your organization's billing settings, or write to ${contact}.`,
    proChangedBody: (version, accepted) =>
      `Version ${version} of the Terms of Service replaces the version you accepted (${accepted}). As our email announced, it applies to everyone who keeps using romulus after it takes effect. Read it, then accept it to continue.`,
    proChangedRefuse: (contact) =>
      `If you do not accept it, you can terminate the contract free of charge before it takes effect: cancel your subscription in your organization's billing settings, or write to ${contact}. The prepaid fees for the unused part of the billing period are refunded.`,
    termsBefore: "I have read and accept the",
    termsLink: "Terms of Service",
    termsAfter: (version) => `(version ${version}).`,
    privacyBefore: "How we handle your personal data:",
    privacyLink: "Privacy Policy",
    newTab: "(opens in a new tab)",
    missing: "Accept the Terms of Service to continue.",
    submit: "Continue to romulus",
    later: "Not now",
    signOut: "Sign out",
    language: "Language",
    versionChanged:
      "The Terms changed while you were reading. Read the new version, then accept it.",
    failed: "Could not save your acceptance. Check your connection and try again.",
    signOutFailed: "Could not sign out. Try again.",
  },
  fr: {
    firstTitle: "Acceptez les Conditions générales",
    firstBody:
      "Pour utiliser romulus, avec l'offre gratuite (Free) ou une offre payante souscrite plus tard, acceptez ses Conditions générales d'utilisation et de vente. Elles forment le contrat entre vous et romulus.",
    proTitle: "Avant de continuer",
    proIntro: "romulus est réservé à un usage professionnel.",
    professionalUse: boxes.professional.fr.professional_use,
    personalBefore: "Pour un projet personnel, vous pouvez exploiter vous-même le",
    personalLink: "logiciel libre",
    personalAfter: ".",
    proMissing: "Confirmez que vous utiliserez romulus à des fins professionnelles.",
    proSubmit: "Continuer",
    changedTitle: "Les Conditions générales ont changé",
    changedBody: (version, accepted) =>
      `La version ${version} est en vigueur. Lisez-la, puis acceptez-la pour continuer à utiliser romulus. D'ici là, la version que vous avez acceptée (${accepted}) s'applique jusqu'à la fin de votre période de facturation.`,
    changedRefuse: (contact) =>
      `Si vous n'acceptez pas la nouvelle version, vous pouvez mettre fin au contrat sans frais : résiliez votre abonnement dans les paramètres de facturation de votre organisation, ou écrivez à ${contact}.`,
    proChangedBody: (version, accepted) =>
      `La version ${version} des Conditions générales remplace celle que vous avez acceptée (${accepted}). Comme annoncé par e-mail, elle s'applique à toute personne qui continue d'utiliser romulus après son entrée en vigueur. Lisez-la, puis acceptez-la pour continuer.`,
    proChangedRefuse: (contact) =>
      `Si vous ne l'acceptez pas, vous pouvez résilier le contrat sans frais avant son entrée en vigueur : résiliez votre abonnement dans les paramètres de facturation de votre organisation, ou écrivez à ${contact}. Les sommes payées d'avance pour la partie non écoulée de la période de facturation vous sont remboursées.`,
    termsBefore: "J'ai lu et j'accepte les",
    termsLink: "Conditions générales d'utilisation et de vente",
    termsAfter: (version) => `(version ${version}).`,
    privacyBefore: "Le traitement de vos données personnelles est décrit dans notre",
    privacyLink: "Politique de confidentialité",
    newTab: "(s'ouvre dans un nouvel onglet)",
    missing: "Acceptez les Conditions générales pour continuer.",
    submit: "Continuer vers romulus",
    later: "Plus tard",
    signOut: "Se déconnecter",
    language: "Langue",
    versionChanged:
      "Les Conditions ont changé pendant votre lecture. Lisez la nouvelle version, puis acceptez-la.",
    failed: "Impossible d'enregistrer votre acceptation. Vérifiez votre connexion et réessayez.",
    signOutFailed: "Impossible de se déconnecter. Réessayez.",
  },
};

/** The account's contract page (`/settings/contract`). */
export interface ContractCopy {
  title: string;
  description: string;
  language: string;
  languageHint: string;
  saved: string;
  saveFailed: string;
  termsTitle: string;
  accepted: (version: string, date: string) => string;
  notAccepted: string;
  terms: string;
  privacy: string;
  dpa: string;
  cancelTitle: string;
  cancelBody: string;
  cancelOrg: (org: string) => string;
  adminsCancel: (org: string) => string;
  noOrg: string;
  account: (contact: string) => string;
}

export const contractCopy: Record<Locale, ContractCopy> = {
  en: {
    title: "Your contract",
    description: "The Terms you accepted, the language of your contract, and how to end it.",
    language: "Language",
    languageHint: "Of your contract, the billing pages and the billing and contract emails.",
    saved: "Language saved",
    saveFailed: "Could not save the language. Try again.",
    termsTitle: "Terms of Service",
    accepted: (version, date) => `You accepted version ${version} on ${date}.`,
    notAccepted: "You have not accepted the Terms in force yet.",
    terms: "Terms of Service",
    privacy: "Privacy Policy",
    dpa: "Data Processing Agreement",
    cancelTitle: "Cancel your contract",
    cancelBody:
      "A paid plan is cancelled in its organization's billing settings, by one of its admins. It ends with the period paid for; nothing is deleted.",
    cancelOrg: (org) => `Cancel ${org}'s subscription`,
    adminsCancel: (org) => `Only the admins of ${org} can cancel its subscription.`,
    noOrg: "You are in no organization.",
    account: (contact) =>
      `Your account itself, on the Free plan, costs nothing and never renews. To end it completely, ask us to delete your account: write to ${contact}.`,
  },
  fr: {
    title: "Votre contrat",
    description:
      "Les Conditions que vous avez acceptées, la langue de votre contrat, et comment y mettre fin.",
    language: "Langue",
    languageHint:
      "De votre contrat, des pages de facturation et des e-mails de facturation et de contrat.",
    saved: "Langue enregistrée",
    saveFailed: "Impossible d'enregistrer la langue. Réessayez.",
    termsTitle: "Conditions générales",
    accepted: (version, date) => `Vous avez accepté la version ${version} le ${date}.`,
    notAccepted: "Vous n'avez pas encore accepté les Conditions en vigueur.",
    terms: "Conditions générales",
    privacy: "Politique de confidentialité",
    dpa: "Accord de traitement des données (en anglais)",
    cancelTitle: "Résilier votre contrat",
    cancelBody:
      "Une offre payante se résilie dans les paramètres de facturation de son organisation, par l'un de ses administrateurs. Elle prend fin avec la période payée ; rien n'est supprimé.",
    cancelOrg: (org) => `Résilier l'abonnement de ${org}`,
    adminsCancel: (org) => `Seuls les administrateurs de ${org} peuvent résilier son abonnement.`,
    noOrg: "Vous n'êtes dans aucune organisation.",
    account: (contact) =>
      `Votre compte lui-même, sur l'offre gratuite (Free), ne coûte rien et n'est jamais reconduit. Pour y mettre fin entièrement, demandez-nous la suppression de votre compte en écrivant à ${contact}.`,
  },
};
