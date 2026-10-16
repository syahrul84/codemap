<?php

namespace App\Http\Controllers;

use App\Models\Mall;
use Illuminate\Http\JsonResponse;
use Illuminate\Http\Request;
use Illuminate\Support\Facades\Cache;

class NearbyMallController extends Controller
{
    // Cards are built in steps: a helper, a cache, filters, then extra keys.
    // The app reads `name`, which none of the steps adds.
    public function index(Request $request): JsonResponse
    {
        if ($request->filled('lat')) {
            $cards = $this->cards(Mall::query()->get());
        } else {
            $cards = Cache::remember('malls.index', 900, fn () => $this->cards(Mall::query()->orderBy('name_en')->get()));
        }
        $cards = array_values(array_filter($cards, fn (array $c) => $c['city'] !== null));
        $cards = array_slice($cards, 0, 10);
        $cards = array_map(fn (array $c) => $c + ['is_saved' => false], $cards);

        return response()->json([
            'data' => array_values($cards),
            'meta' => ['total' => count($cards)],
        ]);
    }

    private function cards($malls): array
    {
        return $malls->map(fn (Mall $m) => [
            'id' => $m->id,
            'name_en' => $m->name_en,
            'city' => $m->city,
        ])->all();
    }
}
