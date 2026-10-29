# Flight parser

This package releases flights first. Concert/event tickets and other reservation types remain a separate sub-slice and do not produce flight records.

The deterministic parser supports explicitly labelled text and equivalent HTML blocks, as shown in `fixtures/`. It requires an airline code, confirmation reference, booking date, named passengers, and one or more named itinerary segments. Each segment requires a flight number, airport pair, scheduled departure, current departure and arrival with explicit UTC offsets, and status. It does not guess time zones or extract arbitrary airline prose. Unsupported messages retain ordinary email rendering.

A record represents one booking segment. Identity uses booking airline, confirmation, booking date, and the stable segment reference, such as `outbound` or `return`. Flight number, route and departure date may change without creating another record when these booking identifiers remain the same. Reused confirmation references on a different booking date produce separate records. The parser cannot reconcile a reissued booking reference or an omitted/renamed segment reference automatically.

UTC fields drive dated projections. Local departure/arrival strings preserve each airport's explicit offset for native rows, cards and details. Confirmation, passenger and time fields retain their source evidence. The parser makes no payment or check-in attention inference. `confirmed`, `scheduled` and `on time` normalize to `scheduled`; delayed, cancelled and landed states are supported. Cancellation or landing completes the record, independently of Ack.

Update keys hash flight number, route, UTC departure/arrival and normalized status. They exclude delivery identity, confirmation details, passenger ordering and UTC-offset spelling. Email and remote reports reuse the same update key and first occurrence time. Redelivery attaches evidence without overwriting a newer distinct change. Ack, source-mail archiving, dated projection, completion retention and reactivation are owned by the host.

## Optional remote schedule checks

The package requests Amadeus On-Demand Flight Status at most every 30 minutes through the host's existing service approval and refresh controls. It works from email alone without approval. Requests contain only carrier, flight number and scheduled local departure date, never booking references or passenger names. Production origin is `https://api.amadeus.com`, restricted to `/v2/schedule/flights`. The user supplies a scoped bearer token through the host's credential controls; automatic OAuth token acquisition/renewal is not implemented by this plugin.

The adapter accepts the documented `data`, `flightDesignator` and `flightPoints` response shape. It verifies the requested resource, carrier/number, departure date, airport pair and scheduled departure against the single host-supplied record. Duplicate or mismatched flights and missing/invalid times produce no changes. STD/STA supply scheduled times, ETD/ETA supply estimates and ATD/ATA supply actual times. An actual arrival marks the flight landed. Remote responses without a host-issued `service.fetchedAt` timestamp are ignored. Provider delay-duration-only fields, cancellations without timing evidence, gate changes and terminal changes are not interpreted in this slice.

Reference response shape: https://github.com/amadeus4dev/data-collection/blob/master/data/ondemandflightstatus.md

## Release checks

`confirmation.json`, `itinerary.json`, `changed-itinerary.json`, `delay.json`, `further-delay.json` and `amadeus-delay.json` cover confirmation evidence, multiple segments, itinerary changes and cross-transport delay identity. Plugin unit tests reject incomplete or ambiguous inputs and verify email-only operation. `core/src/store/tests/flight_tests.rs` executes the built WASM against durable host storage to check Ack, Coming up without reminders, unapproved refresh suppression, email/remote deduplication, archived duplicates and distinct-change reactivation. `FlightTests.swift` checks native row/card/detail rendering with offsets and evidence.
