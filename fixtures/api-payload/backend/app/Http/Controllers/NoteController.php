<?php

namespace App\Http\Controllers;

use Illuminate\Http\Request;

class NoteController extends Controller
{
    public function store(Request $request)
    {
        $body = $request->input('body');
        $pinned = $request->boolean('pinned');
        $color = $request['color'];
        $project = $this->project();

        return response()->json(['data' => compact('body', 'pinned', 'color', 'project')]);
    }

    public function index(Request $request)
    {
        $search = $request->input('search');

        return response()->json(['data' => app('notes')->where('body', $search)->paginate()]);
    }

    private function project()
    {
        request()->validate(['project_id' => 'required']);

        return request()->input('project_id');
    }
}
