// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Re-export Payload-generated types so landing/web imports stay
// short and the relative path to `cms/payload-types.ts` lives in
// exactly one place.

export type {
  Advantage,
  Booking,
  BoringTech,
  BootstrapStrip,
  Comparison,
  FooterContent,
  LandingHero,
  LandingTransparency,
  LegalPrivacy,
  LegalTerm,
  NotFound,
  Roadmap,
  ScalingJourney,
  SiteSetting,
  TierLadder,
  ValueProp,
  WaitlistFormCopy,
  WaitlistSignup,
} from '../../../cms/payload-types';
